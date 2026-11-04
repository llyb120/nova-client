//! Parameter validation shared by the narrow browser and desktop tools.
use serde_json::Value;

// shortcut: only bundled automation schema keywords; extend this validator and its vocabulary tests before adding others.
pub(crate) fn check(schema: &Value, value: &Value, path: &str) -> Result<Value, String> {
    let matches = match schema["type"].as_str() {
        Some("object") => value.is_object(), Some("array") => value.is_array(), Some("string") => value.is_string(),
        Some("integer") => value.is_i64() || value.is_u64(), Some("number") => value.is_number(),
        Some("boolean") => value.is_boolean(), Some("null") => value.is_null(), None => true,
        Some(kind) => return Err(format!("不支持的自动化工具 schema 类型：{kind}")),
    };
    if !matches { return Err(format!("{path} 应为 {}", schema["type"])); }
    if schema["enum"].as_array().is_some_and(|choices| !choices.contains(value)) {
        return Err(format!("{path} 只能是 {}", schema["enum"]));
    }
    if let Some(n) = value.as_f64() {
        for (key, invalid) in [("minimum", false), ("exclusiveMinimum", true), ("maximum", false)] {
            if let Some(bound) = schema[key].as_f64() {
                if if key == "maximum" { n > bound } else if invalid { n <= bound } else { n < bound } {
                    return Err(format!("{path} 不符合 {key}={bound}"));
                }
            }
        }
    }
    let length = value.as_str().map(|s| (s.chars().count(), "minLength", "maxLength"))
        .or_else(|| value.as_array().map(|a| (a.len(), "minItems", "maxItems")));
    if let Some((n, min, max)) = length {
        if schema[min].as_u64().is_some_and(|v| (n as u64) < v) || schema[max].as_u64().is_some_and(|v| n as u64 > v) {
            return Err(format!("{path} 长度超出允许范围"));
        }
    }
    let mut checked = value.clone();
    if let (Some(values), Some(items)) = (value.as_array(), schema.get("items")) {
        checked = values.iter().enumerate().map(|(i, v)| check(items, v, &format!("{path}[{i}]")))
            .collect::<Result<Vec<_>, _>>().map(Value::Array)?;
    }
    if let Some(values) = value.as_object() {
        let required = schema["required"].as_array().cloned().unwrap_or_default();
        let props = &schema["properties"];
        let mut out = serde_json::Map::new();
        for (key, v) in values {
            let Some(s) = props.get(key) else {
                if schema["additionalProperties"] == false { return Err(format!("{path} 不接受字段 {key}")); }
                out.insert(key.clone(), v.clone()); continue;
            };
            let optional = !required.iter().any(|k| k == key);
            let empty = (v == "" && s["minLength"].as_u64().is_some_and(|n| n > 0))
                || (v.as_array().is_some_and(Vec::is_empty) && s["minItems"].as_u64().is_some_and(|n| n > 0))
                || (v.as_f64() == Some(0.) && s["minimum"].as_f64().is_some_and(|n| n >= 1.));
            if optional && (v.is_null() || empty) { continue; }
            out.insert(key.clone(), check(s, v, &format!("{path}.{key}"))?);
        }
        for key in required.iter().filter_map(Value::as_str) {
            if !out.contains_key(key) { return Err(format!("{path} 缺少必填字段 {key}")); }
        }
        checked = Value::Object(out);
    }
    // anyOf adds constraints to its siblings; a matching branch must never bypass common validation.
    if let Some(branches) = schema["anyOf"].as_array() {
        let mut errors = Vec::new();
        for branch in branches {
            match check(branch, &checked, path) { Ok(value) => return Ok(value), Err(error) => errors.push(error) }
        }
        for (branch, error) in branches.iter().zip(&errors) {
            if branch["properties"].as_object().is_some_and(|props| props.iter().any(|(key, s)|
                s["enum"].as_array().is_some_and(|choices| choices.len() == 1 && choices.contains(&checked[key])))) {
                return Err(error.clone());
            }
        }
        return Err(format!("{path} 不符合允许的结构：{}", errors.join("；")));
    }
    Ok(checked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn any_of_keeps_common_object_constraints_and_normalizes_before_branch_requirements() {
        let schema = json!({"type":"object","properties":{
            "mode":{"type":"string"},"snapshotId":{"type":"string","minLength":1},
            "count":{"type":"integer","minimum":0},"enabled":{"type":"boolean"},"text":{"type":"string"}},
            "required":["mode"],"additionalProperties":false,"anyOf":[
                {"type":"object","properties":{"mode":{"type":"string","enum":["wait"]}}},
                {"type":"object","properties":{"snapshotId":{"type":"string","minLength":1}},"required":["snapshotId"]}
            ]});
        let valid=json!({"mode":"wait","count":0,"enabled":false,"text":""});
        assert_eq!(check(&schema,&valid,"参数").unwrap(),valid);
        assert_eq!(check(&schema,&json!({"mode":"wait","snapshotId":null}),"参数").unwrap(),json!({"mode":"wait"}));
        assert_eq!(check(&schema,&json!({"mode":"input","snapshotId":"s"}),"参数").unwrap()["snapshotId"],"s");
        for invalid in [json!({"mode":"wait","extra":1}),json!({"mode":"wait","count":-1}),json!({"mode":"wait","enabled":0}),
            json!({"snapshotId":"s"}),json!({"mode":"input"}),json!({"mode":"input","snapshotId":null}),json!({"mode":"input","snapshotId":""})] {
            assert!(check(&schema,&invalid,"参数").is_err(),"{invalid}");
        }
    }

    #[test]
    fn any_of_keeps_common_scalar_and_array_limits() {
        let scalar=json!({"type":"integer","maximum":4,"anyOf":[{"enum":[0]},{"minimum":1}]});
        for value in [json!(0),json!(4)] { assert_eq!(check(&scalar,&value,"参数").unwrap(),value); }
        for value in [json!(-1),json!(1.5),json!(5),json!("1")] { assert!(check(&scalar,&value,"参数").is_err()); }
        let array=json!({"type":"array","minItems":1,"maxItems":2,"items":{"type":"integer"},"anyOf":[{"type":"array"}]});
        assert_eq!(check(&array,&json!([0,1]),"参数").unwrap(),json!([0,1]));
        for value in [json!([]),json!([1,2,3]),json!([true])] { assert!(check(&array,&value,"参数").is_err()); }
    }
}
