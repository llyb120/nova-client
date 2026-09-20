"""Balance natural-language behavioral constraints before code ranking.
The baseline is the accepted literal-recall production source. This does not
change output budgets, dependency expansion, model defaults, or Reasonix.
"""
from pathlib import Path
import hashlib
ROOT=Path("src-tauri/src/nova_tools_native")
def load(name,expected):
    p=ROOT/name;b=p.read_bytes()
    got=hashlib.sha1(b"blob "+str(len(b)).encode()+b"\0"+b).hexdigest()
    assert got==expected,(name,got)
    return p,b.decode("utf-8")
def sub(s,a,b):
    assert s.count(a)==1,a[:160]
    return s.replace(a,b)

p,s=load("polaris_query.rs","9c6c6d25774e5838c24c023b7ab5818be183d64d")
# "reuse existing" and "restart" are behaviorally different from generic cache/start.
s=sub(s,
    '"缓存|复用|cache|memo|reuse",',
    '"缓存|复用|cache|memo|reuse|remount",')
s=sub(s,
    '"启动|重启|start|launch|startup|restart",',
    '"启动|start|launch|startup", "重新启动|重启|restart",')
# Behavioral constraints should not be outvoted by generic object names.
s=sub(s,
    '''let strong = ["cancel", "encrypt", "decrypt", "retry", "dedup", "delete", "restore", "persist", "copy", "paste", "close", "hold", "convert", "remember"];''',
    '''let strong = ["cancel", "encrypt", "decrypt", "retry", "dedup", "delete", "restore", "persist", "copy", "paste", "close", "hold", "convert", "remember",
            "inherit", "expand", "hide", "reuse", "remount", "retain", "preserve", "restart", "pause", "resume"];''')
# Negative/contrast language is especially discriminative: "not restart" is
# not equivalent to "start". Boost only the matched concept group, never names.
needle='''        let predicates = CONCEPTS.iter().filter(|group| {
            group.split('|').any(|s| original.contains(s)) && group.split('|').any(|s| strong.contains(&s))
        }).collect::<Vec<_>>();
        for (term, weight) in &mut terms {
            if predicates.iter().any(|group| group.split('|').any(|s| s == term.as_str())) { *weight *= 3.0; }
            else if !predicates.is_empty() && ["click", "coordinate", "pointer", "点击", "坐标"].contains(&term.as_str()) { *weight *= 0.5; }
        }'''
replacement='''        let predicates = CONCEPTS.iter().filter(|group| {
            group.split('|').any(|s| original.contains(s)) && group.split('|').any(|s| strong.contains(&s))
        }).collect::<Vec<_>>();
        let lower_task=task.to_lowercase();
        let constrained = CONCEPTS.iter().filter(|group| {
            group.split('|').any(|alias| {
                let alias_lower=alias.to_lowercase();
                lower_task.match_indices(&alias_lower).map(|(i,_)|i).any(|pos| {
                    let start=lower_task.floor_char_boundary(pos.saturating_sub(24));
                    let before=&lower_task[start..pos];
                    ["不","未","没","无","避免","防止","禁止","不能","不会","不要","而不是","instead of","without","never"," not "]
                        .iter().any(|marker|before.contains(marker))
                })
            })
        }).collect::<Vec<_>>();
        for (term, weight) in &mut terms {
            if constrained.iter().any(|group| group.split('|').any(|s| s == term.as_str())) { *weight *= 4.0; }
            else if predicates.iter().any(|group| group.split('|').any(|s| s == term.as_str())) { *weight *= 3.0; }
            else if (!predicates.is_empty()||!constrained.is_empty()) && ["click", "coordinate", "pointer", "点击", "坐标"].contains(&term.as_str()) { *weight *= 0.5; }
        }'''
s=sub(s,needle,replacement)
# Generic tests for contrast semantics, not repository functions.
anchor='''    #[test] fn encryption_direction_is_not_a_generic_credentials_getter() {'''
tests='''    #[test] fn reuse_and_restart_are_distinct_constraints() {
        let q=Query::parse(serde_json::json!({"task":"收起后复用原进程，而不是重新启动"})).unwrap();
        let w=|term:&str|q.terms.iter().find(|(s,_)|s==term).map(|(_,w)|*w).unwrap_or(0.0);
        assert!(w("reuse")>1.0);assert!(w("remount")>1.0);
        assert!(w("restart")>w("start"),"restart={} start={}",w("restart"),w("start"));
    }
    #[test] fn negative_inheritance_is_a_behavioral_constraint() {
        let q=Query::parse(serde_json::json!({"task":"新建页面不会继承上次展开状态"})).unwrap();
        let w=|term:&str|q.terms.iter().find(|(s,_)|s==term).map(|(_,w)|*w).unwrap_or(0.0);
        assert!(w("inherit")>w("state"));assert!(w("expand")>1.0);
    }
'''
s=sub(s,anchor,tests+anchor)
p.write_text(s,encoding="utf-8")

p,s=load("polaris_rank.rs","84888371cb7c02fa4040e3e5bf1147feef495f2b")
old='''            *score*=0.30+0.70*fraction*fraction;'''
new='''            // Distinct requested behaviors matter more than repeated generic
            // names. Keep a floor for sparse evidence, but reward balanced
            // multi-facet coverage much more strongly on complex questions.
            *score*=if facets.len()>=3 {0.16+0.84*fraction*fraction*fraction}
                else {0.30+0.70*fraction*fraction};'''
s=sub(s,old,new)
p.write_text(s,encoding="utf-8")
print("Applied generic behavioral-constraint weighting and balanced facet coverage.")
