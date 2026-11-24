//! Repository tools are resident; desktop and browser operation schemas are returned on demand.
use serde_json::{json, Value};
use std::sync::LazyLock;

const SKILLS: [(&str, &str, &str); 2] = [
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

pub fn mcp_tools() -> Vec<Value> {
    let mut polaris: Value = serde_json::from_str(include_str!("../../../../scripts/polaris-tool.json"))
        .expect("bundled Polaris definition");
    polaris["_meta"] = json!({"antigravity/alwaysLoad":true});
    let mut tools = vec![
        polaris,
        json!({"name":"code_map", "description":"List repository files or symbols for a directory prefix",
            "inputSchema":{"type":"object","properties":{"scope":{"type":"string"}},"additionalProperties":false}, "_meta":{"antigravity/alwaysLoad":true}}),
    ];
    tools.extend(SKILLS.iter().map(|(name, summary, _)| json!({
        "name": name, "description": format!("{summary}. Call with {{}} to read SKILL.md; tool alone shows its schema; tool + input executes."),
        "inputSchema": {"type":"object", "properties": {
            "tool":{"type":"string"}, "input":{"type":"string","description":"Arguments encoded as a JSON object"}
        }, "additionalProperties":false}, "_meta":{"antigravity/alwaysLoad":true}
    })));
    tools
}

static CATALOG: LazyLock<Vec<Value>> = LazyLock::new(|| {
    let mut ops = Vec::new();
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
    let object = args.as_object().ok_or("Expected tool arguments object")?;
    if matches!(skill, "polaris" | "code_map") {
        if skill == "polaris" && args["task"].as_str().is_none_or(|s| s.trim().is_empty()) {
            return Err("Pass the search objective directly as {\"task\":\"...\"}".into());
        }
        return Ok(SkillCall::Execute(skill.to_owned(), args.clone()));
    }
    let (_, _, manual) = SKILLS
        .iter()
        .find(|s| s.0 == skill)
        .ok_or("Unknown Nova skill")?;
    let ops = CATALOG.iter().filter_map(|op| {
        let name = op["name"].as_str()?;
        let belongs = match skill {
            "jianlai" => name.starts_with("jianlai_"),
            "chrome" => name.starts_with("browser_"),
            _ => false,
        };
        belongs.then_some((name, op))
    });
    let args = object;
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
    let input: Value = match input {
        Value::String(s) => serde_json::from_str(s).map_err(|e| e.to_string())?,
        Value::Object(_) => input.clone(),
        _ => return Err("input must be a JSON string or object".into()),
    };
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
        let tools = mcp_tools();
        assert_eq!(tools.iter().map(|t| t["name"].as_str().unwrap()).collect::<Vec<_>>(), ["polaris", "code_map", "jianlai", "chrome"]);
        assert_eq!(tools[0]["inputSchema"]["properties"]["task"]["type"], "string");
        assert!(tools[0]["inputSchema"]["properties"].get("tool").is_none());
        assert_eq!(tools[0]["inputSchema"]["required"], json!(["task"]));
        assert_eq!(tools[0]["inputSchema"]["properties"].as_object().unwrap().len(), 2);
        let args = json!({"task":"Locate model selection", "root":"../other"});
        assert_eq!(tools[0]["inputSchema"]["properties"]["root"]["type"], "string");
        assert!(matches!(resolve("polaris", &args), Ok(SkillCall::Execute(n, v)) if n == "polaris" && v == args));
        assert!(resolve("polaris", &json!({})).is_err());
        assert!(matches!(resolve("code_map", &json!({})), Ok(SkillCall::Execute(n, _)) if n == "code_map"));
        assert!(serde_json::to_vec(&tools).unwrap().len() < 3072);
        for (skill, op) in [
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
            assert!(
                matches!(resolve(skill, &json!({"tool": op, "input":{"action":"list"}})), Ok(SkillCall::Execute(n, v)) if n == op && v["action"] == "list")
            );
        }
        assert!(resolve("jianlai", &json!({"tool":"browser_tabs","input":"{}"})).is_err());
        assert!(resolve("chrome", &json!({"tool":"browser_tabs","input":"[]"})).is_err());
    }
}
