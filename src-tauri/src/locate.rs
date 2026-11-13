//! 剑来与 Chrome 共用的目标绑定：主模型只规划动作和 target 描述，
//! 执行器在最新观察（控件树/DOM + 截图）上取点。能唯一命中树节点就直接用，
//! 否则才走轻量视觉 oneshot，主模型不自己估坐标。

use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;

#[derive(Clone, Debug)]
pub struct TreeItem {
    pub name: String,
    pub role: String,
    pub automation_id: String,
    pub ref_id: Option<String>,
    pub frame: Option<u64>,
    pub center: Option<(i32, i32)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Located {
    Point { x: i32, y: i32, by: &'static str },
    Ref { reference: String, frame: Option<u64>, by: &'static str },
}

#[derive(Debug)]
pub enum TreeMatch<'a> {
    Unique(&'a TreeItem),
    Ambiguous(Vec<String>),
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prefer {
    /// 桌面指针：优先中心点。
    Point,
    /// 网页：优先 DOM ref。
    Ref,
}

#[derive(Debug, Default, Deserialize)]
struct LocateJson {
    #[serde(default)]
    x: Option<f64>,
    #[serde(default)]
    y: Option<f64>,
    #[serde(default)]
    r#ref: Option<String>,
    #[serde(default, alias = "reference")]
    reference: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    frame: Option<u64>,
}

fn normalize(s: &str) -> String {
    s.trim().to_lowercase()
}

pub fn match_tree<'a>(items: &'a [TreeItem], target: &str) -> TreeMatch<'a> {
    let t = normalize(target);
    if t.is_empty() {
        return TreeMatch::None;
    }
    let id_hits: Vec<&'a TreeItem> = items
        .iter()
        .filter(|item| {
            (!item.automation_id.is_empty() && normalize(&item.automation_id) == t)
                || item
                    .ref_id
                    .as_deref()
                    .is_some_and(|r| normalize(r) == t)
        })
        .collect();
    if id_hits.len() == 1 {
        return TreeMatch::Unique(id_hits[0]);
    }
    let exact: Vec<&'a TreeItem> = items
        .iter()
        .filter(|item| !item.name.is_empty() && normalize(&item.name) == t)
        .collect();
    if exact.len() == 1 {
        return TreeMatch::Unique(exact[0]);
    }
    if exact.len() > 1 {
        return TreeMatch::Ambiguous(exact.iter().map(|item| label(item)).collect());
    }
    let contains: Vec<&'a TreeItem> = items
        .iter()
        .filter(|item| {
            let name = normalize(&item.name);
            !name.is_empty()
                && name.chars().count() >= 2
                && (name.contains(&t) || (t.contains(&name) && name.chars().count() >= 2))
        })
        .collect();
    if contains.len() == 1 {
        return TreeMatch::Unique(contains[0]);
    }
    if contains.len() > 1 {
        return TreeMatch::Ambiguous(contains.iter().map(|item| label(item)).collect());
    }
    TreeMatch::None
}

fn label(item: &TreeItem) -> String {
    let id = item
        .ref_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .or(Some(item.automation_id.as_str()).filter(|s| !s.is_empty()))
        .unwrap_or("-");
    if item.name.is_empty() {
        format!("{id} {}", item.role)
    } else {
        format!("{id} {} {}", item.role, item.name)
    }
}

fn first_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escape = false;
    for (i, ch) in text[start..].char_indices() {
        if in_str {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_str = false;
            }
            continue;
        }
        match ch {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..start + i + 1]);
                }
            }
            _ => {}
        }
    }
    None
}

pub fn parse_locate_output(text: &str) -> Result<Value, String> {
    let trimmed = text.trim();
    let blob = first_json_object(trimmed).unwrap_or(trimmed);
    let parsed: LocateJson = serde_json::from_str(blob).map_err(|_| {
        format!("定位结果不是 JSON 坐标或 ref：{}", short(trimmed, 80))
    })?;
    let reference = parsed
        .r#ref
        .or(parsed.reference)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let name = parsed
        .name
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if parsed.x.is_some() != parsed.y.is_some() {
        return Err("定位 JSON 必须同时给出 x 和 y".into());
    }
    if parsed.x.is_none() && reference.is_none() && name.is_none() {
        return Err("定位 JSON 需要 ref、name 或 x/y".into());
    }
    let mut value = json!({});
    if let (Some(x), Some(y)) = (parsed.x, parsed.y) {
        if !x.is_finite() || !y.is_finite() {
            return Err("定位坐标无效".into());
        }
        value["x"] = json!(x.round() as i64);
        value["y"] = json!(y.round() as i64);
    }
    if let Some(r) = reference {
        value["ref"] = json!(r);
    }
    if let Some(n) = name {
        value["name"] = json!(n);
    }
    if let Some(frame) = parsed.frame {
        value["frame"] = json!(frame);
    }
    Ok(value)
}

fn short(text: &str, max: usize) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

fn from_item(item: &TreeItem, prefer: Prefer, by: &'static str) -> Result<Located, String> {
    if prefer == Prefer::Ref {
        if let Some(reference) = item.ref_id.as_deref().filter(|s| !s.is_empty()) {
            return Ok(Located::Ref {
                reference: reference.to_string(),
                frame: item.frame,
                by,
            });
        }
    }
    if let Some((x, y)) = item.center {
        return Ok(Located::Point { x, y, by });
    }
    if let Some(reference) = item.ref_id.as_deref().filter(|s| !s.is_empty()) {
        return Ok(Located::Ref {
            reference: reference.to_string(),
            frame: item.frame,
            by,
        });
    }
    Err("命中的控件没有可用坐标或 ref".into())
}

fn compact_items(items: &[TreeItem]) -> String {
    items
        .iter()
        .take(40)
        .map(|item| {
            let id = item
                .ref_id
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(|s| format!("ref={s}"))
                .or_else(|| {
                    (!item.automation_id.is_empty())
                        .then(|| format!("id={}", item.automation_id))
                })
                .unwrap_or_else(|| "-".into());
            let center = item
                .center
                .map(|(x, y)| format!(" center={x},{y}"))
                .unwrap_or_default();
            format!("- {id} role={} name={}{center}", item.role, json!(item.name))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub async fn locate(
    target: &str,
    items: &[TreeItem],
    image: Option<(&Path, u32, u32)>,
    prefer: Prefer,
) -> Result<Located, String> {
    let target = target.trim();
    if target.is_empty() {
        return Err("target 不能为空".into());
    }
    match match_tree(items, target) {
        TreeMatch::Unique(item) => return from_item(item, prefer, "tree"),
        TreeMatch::Ambiguous(_) | TreeMatch::None => {}
    }
    let Some((path, width, height)) = image else {
        return Err(match match_tree(items, target) {
            TreeMatch::Ambiguous(names) => format!(
                "目标「{target}」有多个匹配（{}），且没有可用截图做 oneshot 消歧",
                names.join("；")
            ),
            _ => format!("目标「{target}」在控件树里没有唯一命中，且没有可用截图做 oneshot 定位"),
        });
    };
    if !path.is_file() {
        return Err(format!("定位截图不存在：{}", path.display()));
    }
    let list = compact_items(items);
    let want = match prefer {
        Prefer::Ref => "能确定控件时只输出 {\"ref\":\"控件ref\"}；否则用图片像素 {\"x\":整数,\"y\":整数}。",
        Prefer::Point => "能确定控件且有中心点时用 {\"x\":整数,\"y\":整数}；网页控件也可用 {\"ref\":\"控件ref\"}。",
    };
    let prompt = format!(
        "图片宽 {width} 高 {height}，坐标原点在左上角，单位是图片像素。\n要定位的目标：{target}\n可用控件：\n{}\n{want}\n只输出一个 JSON 对象，不要其它文字。x 在 0..{}，y 在 0..{}。",
        if list.is_empty() { "(无命名控件，请直接看图取点)".into() } else { list },
        width.saturating_sub(1),
        height.saturating_sub(1)
    );
    let text = crate::lyra::bridge::vision_oneshot(
        "你是界面定位器。根据截图和控件列表找出目标，不要解释，不要规划下一步动作。",
        &prompt,
        path,
    )
    .await?;
    let parsed = parse_locate_output(&text)?;
    if let Some(reference) = parsed["ref"].as_str() {
        if let TreeMatch::Unique(item) = match_tree(items, reference) {
            return from_item(item, prefer, "oneshot");
        }
        return Ok(Located::Ref {
            reference: reference.to_string(),
            frame: parsed["frame"].as_u64(),
            by: "oneshot",
        });
    }
    if let Some(name) = parsed["name"].as_str() {
        if let TreeMatch::Unique(item) = match_tree(items, name) {
            return from_item(item, prefer, "oneshot");
        }
    }
    if let (Some(x), Some(y)) = (parsed["x"].as_i64(), parsed["y"].as_i64()) {
        let x = x.clamp(0, width.saturating_sub(1) as i64) as i32;
        let y = y.clamp(0, height.saturating_sub(1) as i64) as i32;
        return Ok(Located::Point { x, y, by: "oneshot" });
    }
    Err(format!("oneshot 未能定位「{target}」"))
}

pub fn items_from_elements(elements: &Value) -> Vec<TreeItem> {
    elements["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let name = item["name"].as_str().unwrap_or_default().to_string();
            let automation_id = item["automationId"]
                .as_str()
                .or(item["automation_id"].as_str())
                .unwrap_or_default()
                .to_string();
            let ref_id = item["ref"]
                .as_str()
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty());
            if name.is_empty() && automation_id.is_empty() && ref_id.is_none() {
                return None;
            }
            let center = item
                .get("center")
                .and_then(|c| Some((c["x"].as_i64()? as i32, c["y"].as_i64()? as i32)))
                .or_else(|| {
                    let x = item["x"].as_i64()?;
                    let y = item["y"].as_i64()?;
                    let w = item["width"].as_i64().unwrap_or(0);
                    let h = item["height"].as_i64().unwrap_or(0);
                    Some(((x + w / 2) as i32, (y + h / 2) as i32))
                });
            Some(TreeItem {
                name,
                role: item["role"].as_str().unwrap_or_default().to_string(),
                automation_id,
                ref_id,
                frame: item["frame"].as_u64(),
                center,
            })
        })
        .collect()
}

pub fn items_from_pages(pages: &Value) -> Vec<TreeItem> {
    pages["pages"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|page| {
            let frame = page["frame"].as_u64();
            page["items"].as_array().into_iter().flatten().cloned().map(move |mut item| {
                if item.get("frame").is_none() {
                    if let Some(frame) = frame {
                        item["frame"] = json!(frame);
                    }
                }
                item
            })
        })
        .collect::<Vec<_>>()
        .pipe_as_elements()
}

trait PipeElements {
    fn pipe_as_elements(self) -> Vec<TreeItem>;
}

impl PipeElements for Vec<Value> {
    fn pipe_as_elements(self) -> Vec<TreeItem> {
        items_from_elements(&json!({"items": self}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str, id: &str, reference: Option<&str>, center: Option<(i32, i32)>) -> TreeItem {
        TreeItem {
            name: name.into(),
            role: "button".into(),
            automation_id: id.into(),
            ref_id: reference.map(|s| s.into()),
            frame: Some(0),
            center,
        }
    }

    #[test]
    fn parse_accepts_bare_and_fenced_json() {
        let a = parse_locate_output("{\"x\": 10.4, \"y\": 20.6}").unwrap();
        assert_eq!((a["x"].clone(), a["y"].clone()), (json!(10), json!(21)));
        let b = parse_locate_output("here is the point\n```json\n{\"ref\":\"e12\",\"frame\":0}\n```").unwrap();
        assert_eq!(b["ref"], "e12");
        assert_eq!(b["frame"], 0);
        assert!(parse_locate_output("nope").is_err());
        assert!(parse_locate_output("{\"x\":1}").is_err());
    }

    #[test]
    fn tree_match_prefers_unique_name_and_reports_ambiguity() {
        let items = vec![
            item("保存", "save", Some("e1"), Some((10, 10))),
            item("取消", "cancel", Some("e2"), Some((40, 10))),
            item("保存为", "saveAs", Some("e3"), Some((70, 10))),
        ];
        match match_tree(&items, "取消") {
            TreeMatch::Unique(hit) => assert_eq!(hit.ref_id.as_deref(), Some("e2")),
            _ => panic!("expected unique"),
        }
        match match_tree(&items, "e1") {
            TreeMatch::Unique(hit) => assert_eq!(hit.name, "保存"),
            _ => panic!("expected id"),
        }
        match match_tree(&items, "保存") {
            TreeMatch::Unique(hit) => assert_eq!(hit.ref_id.as_deref(), Some("e1"), "exact name wins over a longer contains match"),
            other => panic!("expected unique exact 保存, got {other:?}"),
        }
        assert!(matches!(match_tree(&items, "保"), TreeMatch::Ambiguous(_)), "shared substring stays ambiguous");
        assert!(matches!(match_tree(&items, "没有这个"), TreeMatch::None));
    }
}
