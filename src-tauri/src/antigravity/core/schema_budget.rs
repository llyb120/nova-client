//! Only the three skill entry points are resident. Manuals and operation schemas are returned on demand.
use serde_json::{json, Value};
use std::sync::LazyLock;

const SKILLS: [(&str, &str, &str); 3] = [
    (
        "polaris",
        "Search the current repository",
        include_str!("../skills/polaris/SKILL.md"),
    ),
    (
        "jianlai",
        "Operate native desktop applications",
        include_str!("../skills/jianlai/SKILL.md"),
    ),
    (
        "chrome",
        "Operate the Nova browser (not agy's built-in browser)",
        include_str!("../skills/chrome/SKILL.md"),
    ),
];

pub fn skill_tools() -> Vec<Value> {
    SKILLS.iter().map(|(name, summary, _)| json!({
        "name": name, "description": format!("{summary}. Call with {{}} to read SKILL.md; tool alone shows its schema; tool + input executes."),
        "inputSchema": {"type":"object", "properties": {
            "tool":{"type":"string"}, "input":{"type":"string","description":"Arguments encoded as a JSON object"}
        }, "additionalProperties":false}, "_meta":{"antigravity/alwaysLoad":true}
    })).collect()
}

static CATALOG: LazyLock<Vec<Value>> = LazyLock::new(|| {
    let mut ops = vec![
        json!({"name":"polaris", "description":"Search for relevant source and symbols", "parameters":{"type":"object","properties":{
            "keywords":{"type":"array","items":{"type":"string"}}, "task":{"type":"string"},
            "files":{"type":"array","items":{"type":"string"}}, "deep":{"type":"boolean"}, "coupling":{"type":"boolean"},
            "budget":{"type":"integer","minimum":100,"maximum":1200},"maxBytes":{"type":"integer","minimum":4096,"maximum":65536}
        }}}),
        json!({"name":"code_map", "description":"List repository files or symbols for a directory prefix", "parameters":{"type":"object","properties":{"scope":{"type":"string"}}}}),
    ];
    for defs in [
        include_str!("../../../../scripts/browser-tools.json"),
        include_str!("../../../../scripts/jianlai-tools.json"),
    ] {
        ops.extend(serde_json::from_str::<Vec<Value>>(defs).expect("bundled Nova tool catalog"));
    }
    ops
});

pub enum SkillCall {
    Read(String),
    Execute(String, Value),
}

pub fn resolve(skill: &str, args: &Value) -> Result<SkillCall, String> {
    let (_, _, manual) = SKILLS
        .iter()
        .find(|s| s.0 == skill)
        .ok_or("Unknown Nova skill")?;
    let ops = CATALOG.iter().filter_map(|op| {
        let name = op["name"].as_str()?;
        let belongs = match skill {
            "polaris" => matches!(name, "polaris" | "code_map"),
            "jianlai" => name.starts_with("jianlai_"),
            "chrome" => name.starts_with("browser_"),
            _ => false,
        };
        belongs.then_some((name, op))
    });
    let args = args.as_object().ok_or("Expected skill arguments object")?;
    if args.is_empty() {
        let names = ops.map(|(name, _)| name).collect::<Vec<_>>().join(", ");
        return Ok(SkillCall::Read(format!("{manual}\nOperations: {names}")));
    }
    if args.keys().any(|key| key != "tool" && key != "input") {
        return Err("Expected tool and optional input".into());
    }
    let name = args
        .get("tool")
        .and_then(Value::as_str)
        .ok_or("Expected tool name")?;
    let (_, spec) = ops
        .into_iter()
        .find(|(n, _)| *n == name)
        .ok_or("Unknown operation for this skill")?;
    let Some(input) = args.get("input") else {
        return Ok(SkillCall::Read(spec.to_string()));
    };
    let input: Value = serde_json::from_str(input.as_str().ok_or("input must be a JSON string")?)
        .map_err(|e| e.to_string())?;
    if !input.is_object() {
        return Err("input must encode a JSON object".into());
    }
    Ok(SkillCall::Execute(name.to_owned(), input))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compact_index_and_lazy_dispatch() {
        let tools = skill_tools();
        assert_eq!(tools.len(), 3);
        assert!(serde_json::to_vec(&tools).unwrap().len() < 2048);
        for (skill, op) in [
            ("polaris", "code_map"),
            ("chrome", "browser_tabs"),
            ("jianlai", "jianlai_windows"),
        ] {
            assert!(
                matches!(resolve(skill, &json!({})), Ok(SkillCall::Read(s)) if s.contains("---") && s.contains(op))
            );
            assert!(
                matches!(resolve(skill, &json!({"tool": op})), Ok(SkillCall::Read(s)) if s.contains("parameters"))
            );
            assert!(
                matches!(resolve(skill, &json!({"tool": op, "input":"{\"action\":\"list\"}"})), Ok(SkillCall::Execute(n, v)) if n == op && v["action"] == "list")
            );
        }
        assert!(resolve("jianlai", &json!({"tool":"browser_tabs","input":"{}"})).is_err());
        assert!(resolve("chrome", &json!({"tool":"browser_tabs","input":"[]"})).is_err());
    }
}
