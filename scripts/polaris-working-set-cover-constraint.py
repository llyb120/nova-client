"""Tighten the generated constraint detector after intent balancing.
Only a concept directly adjacent to a negation/contrast marker is constrained.
"""
from pathlib import Path
p=Path("src-tauri/src/nova_tools_native/polaris_query.rs")
s=p.read_text(encoding="utf-8")
old='''        let lower_task=task.to_lowercase();
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
        }).collect::<Vec<_>>();'''
new='''        let lower_task=task.to_lowercase();
        let constrained = CONCEPTS.iter().filter(|group| {
            group.split('|').any(|alias| {
                let alias=alias.to_lowercase();
                if alias.is_empty(){return false;}
                if alias.is_ascii() {
                    ["not ","never ","without ","instead of "].iter()
                        .any(|marker|lower_task.contains(&format!("{marker}{alias}")))
                } else {
                    ["不","未","没","无","避免","防止","禁止","不能","不会","不要","而不是"].iter()
                        .any(|marker|lower_task.contains(&format!("{marker}{alias}")))
                }
            })
        }).collect::<Vec<_>>();'''
assert s.count(old)==1
p.write_text(s.replace(old,new),encoding="utf-8")
print("Scoped constraint weighting to directly negated/contrasted concepts.")
