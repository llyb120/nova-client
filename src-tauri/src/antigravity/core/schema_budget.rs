//! Repository tools exposed by the Nova MCP bridge (`polaris`, `code_map`).
use serde_json::{json, Value};

pub fn mcp_tools(polaris_enabled: bool) -> Vec<Value> {
    let mut tools = Vec::new();
    if polaris_enabled {
        let mut polaris: Value = serde_json::from_str(include_str!("../../../../scripts/polaris-tool.json"))
            .expect("bundled Polaris definition");
        polaris["_meta"] = json!({"antigravity/alwaysLoad":true});
        tools.push(polaris);
    }
    tools.push(json!({"name":"code_map", "description":"List repository files or symbols for a directory prefix",
        "inputSchema":{"type":"object","properties":{"scope":{"type":"string"}},"additionalProperties":false}, "_meta":{"antigravity/alwaysLoad":true}}));
    tools
}

pub fn resolve(tool: &str, args: &Value) -> Result<(String, Value), String> {
    if !args.is_object() {
        return Err("Expected tool arguments object".into());
    }
    match tool {
        "polaris" => {
            if args["task"].as_str().is_none_or(|s| s.trim().is_empty()) {
                return Err("Pass the search objective directly as {\"task\":\"...\"}".into());
            }
            Ok((tool.to_owned(), args.clone()))
        }
        "code_map" => Ok((tool.to_owned(), args.clone())),
        _ => Err(format!("Unknown Nova MCP tool: {tool}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exposes_only_repository_tools_and_rejects_removed_skills() {
        assert_eq!(mcp_tools(false).iter().map(|t| t["name"].as_str().unwrap()).collect::<Vec<_>>(), ["code_map"]);
        let tools = mcp_tools(true);
        assert_eq!(tools.iter().map(|t| t["name"].as_str().unwrap()).collect::<Vec<_>>(), ["polaris", "code_map"]);
        assert_eq!(tools[0]["inputSchema"]["properties"]["task"]["type"], "string");
        assert!(tools[0]["inputSchema"]["properties"].get("tool").is_none());
        assert_eq!(tools[0]["inputSchema"]["required"], json!(["task"]));
        assert_eq!(tools[0]["inputSchema"]["properties"].as_object().unwrap().len(), 2);
        let args = json!({"task":"Locate model selection", "root":"../other"});
        assert_eq!(tools[0]["inputSchema"]["properties"]["root"]["type"], "string");
        assert_eq!(resolve("polaris", &args), Ok(("polaris".into(), args)));
        assert!(resolve("polaris", &json!({})).is_err());
        assert_eq!(resolve("code_map", &json!({})), Ok(("code_map".into(), json!({}))));
        assert!(resolve("jianlai", &json!({})).is_err());
        assert!(resolve("chrome", &json!({})).is_err());
        assert!(resolve("browser_tabs", &json!({})).is_err());
        assert!(resolve("jianlai_windows", &json!({})).is_err());
        assert!(serde_json::to_vec(&tools).unwrap().len() < 3072);
    }
}
