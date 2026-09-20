"""Source-derived component edges and relevant execution callers. No benchmark paths."""
from pathlib import Path
R=Path('src-tauri/src/nova_tools_native')
def edit(name,old,new):
 p=R/name;s=p.read_text();assert s.count(old)==1,(name,old[:90]);p.write_text(s.replace(old,new))
edit('polaris_demand_v2.rs','    for _ in 0..2{','    for expansion in 0..3{')
edit('polaris_demand_v2.rs','let extra=neighbours.iter().take(8)', 'let extra=neighbours.iter().take(if expansion<2{8}else{4})')
edit('polaris_demand_v2.rs','if caller_names.iter().any(|(file,name)|row.file==*file&&body.contains(name)){score+=3000.0;}','if caller_names.iter().any(|(file,name)|row.file==*file&&body.contains(name)){score+=3000.0*q.focus.score(std::iter::once(row.file.as_str()).chain(file.source.iter().take(5).map(String::as_str)).chain(std::iter::once(body.as_str())));}')
# Parse JSX syntax only for selected declarations, never treat comments or
# quoted HTML as a callable reference, never apply JSX parsing to Rust generics.
helper='''fn component_references(text:&str)->(HashSet<String>,Vec<(String,String)>) {
    let mut calls=HashSet::new();let mut members=Vec::new();
    if !text.contains('<'){return (calls,members);}
    let mut parser=tree_sitter::Parser::new();
    if parser.set_language(&tree_sitter_typescript::LANGUAGE_TSX.into()).is_err(){return (calls,members);}
    #[allow(deprecated)] parser.set_timeout_micros(20_000);
    let bounded=text.chars().take(20000).collect::<String>();
    let Some(tree)=parser.parse(&bounded,None)else{return (calls,members);};
    let mut cursor=tree.walk();let mut depth=0usize;
    loop {
        let node=cursor.node();
        if matches!(node.kind(),"jsx_opening_element"|"jsx_self_closing_element") {
            if let Some(name)=node.child_by_field_name("name").and_then(|n|n.utf8_text(bounded.as_bytes()).ok()) {
                if name.chars().next().is_some_and(|c|c.is_ascii_uppercase()) {
                    if let Some((object,member))=name.split_once('.') {calls.insert(object.into());members.push((object.into(),member.into()));}
                    else {calls.insert(name.into());}
                }
            }
            if calls.len()>=64{break;}
        }
        if cursor.goto_first_child(){depth+=1;continue;}
        loop {if cursor.goto_next_sibling(){break;}if depth==0{return (calls,members);}cursor.goto_parent();depth-=1;}
    }
    (calls,members)
}
'''
p=R/'polaris_index.rs';s=p.read_text();s=helper+s;p.write_text(s)
edit('polaris_index.rs','        let owner_references=references(&source[begin-1..finish].join("\\n"));','''        let owner_body=source[begin-1..finish].join("\\n");
        let mut owner_references=references(&owner_body);
        if matches!(file.rsplit('.').next(),Some("tsx"|"jsx")) {
            let (components,members)=component_references(&owner_body);
            owner_references.0.extend(components);owner_references.3.extend(members);
        }''')
edit('polaris_packet.rs','if depth>=2||seed_links>=6','if depth>=3||seed_links>=8')
edit('polaris_packet.rs','if seed_links>=6{break;}','if seed_links>=8{break;}')
edit('polaris_packet.rs','                if let Some(role)=rel{','''                if let Some(mut role)=rel{
                    // A Result<()> signature is not an invocation of its type alias.
                    if role=="callee-reference"&&data_definition(u){role="type-reference";}''')
edit('polaris_packet.rs','let priority=match role{"callee-reference"|"command-reference"=>3,"type-reference"=>2,_=>1};','let priority=if role=="caller-reference"&&q.focus.active()&&focus_source(q,u)>=0.95{4}else{match role{"callee-reference"|"command-reference"=>3,"type-reference"=>2,_=>1}};')
p=R/'polaris_packet.rs';s=p.read_text();s+='''
#[cfg(test)] mod source_edge_tests {
    use super::*;
    #[test]fn jsx_component_edges_reach_three_hop_implementation(){
        let d=tempfile::tempdir().unwrap();
        fs::write(d.path().join("panel.tsx"),"import { Bridge } from './bridge';\\n// 收起命令行面板再打开时保留进程\\nexport function Panel() { return <Bridge/>; }\\n").unwrap();
        fs::write(d.path().join("bridge.tsx"),"import { Leaf } from './leaf';\\nexport function Bridge() { return <Leaf/>; }\\n").unwrap();
        fs::write(d.path().join("leaf.tsx"),"import { keepProcess } from './process';\\nexport function Leaf() { keepProcess(); return <div/>; }\\n").unwrap();
        fs::write(d.path().join("process.ts"),"let active: unknown;\\nexport function keepProcess() { if (!active) active = launch(); return active; }\\n").unwrap();
        let out=polaris(d.path(),serde_json::json!({"task":"收起命令行面板再打开时保留进程","maxBytes":12000})).unwrap();
        for code in ["function Panel", "function Bridge", "function Leaf", "function keepProcess"]{assert!(out.contains(code),"{code}: {out}");}
        assert!(out.len()<=12000);
    }
    #[test]fn strings_comments_and_native_tags_are_not_component_calls(){
        let d=tempfile::tempdir().unwrap();
        fs::write(d.path().join("panel.tsx"),"function Panel() { const text = '<Imaginary/>'; /* <Ghost/> */ return <Real/><div/>; }\\nfunction Real() { return <span/>; }\\n").unwrap();
        let corpus=index::corpus(d.path(),Instant::now()+Duration::from_secs(3)).unwrap();
        let u=corpus.units.iter().find(|u|u.name=="Panel").unwrap();
        assert!(u.calls.contains("Real"));
        for name in ["Imaginary","Ghost","div"]{assert!(!u.calls.contains(name),"{name}");}
    }
}
''';p.write_text(s)
print('Applied AST component edges, bounded three-hop closure and execution-caller priority.')
