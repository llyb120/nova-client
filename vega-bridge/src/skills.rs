//! Skills 发现 —— 移植 pi-coding-agent `loadSkillsFromDir` 的目录扫描与 frontmatter 解析。
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct Skill {
    pub name: String,
    pub file_path: PathBuf,
    pub base_dir: PathBuf,
    pub disable_model_invocation: bool,
}

pub fn skills_root() -> PathBuf {
    crate::config::alkaid_data_root().join("skills")
}

/// Scan `<root>/<name>/SKILL.md` for each immediate subdirectory.
pub fn load_skills_from_dir(root: &Path) -> Vec<Skill> {
    let mut skills = Vec::new();
    let Ok(entries) = fs::read_dir(root) else { return skills; };
    let mut dirs: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.is_dir()).collect();
    dirs.sort();
    for dir in dirs {
        let skill_md = dir.join("SKILL.md");
        if !skill_md.is_file() {
            continue;
        }
        let Ok(content) = fs::read_to_string(&skill_md) else { continue };
        let (frontmatter, _body) = split_frontmatter(&content);
        let name = frontmatter
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
        if name.is_empty() {
            continue;
        }
        let disable = frontmatter.get("disableModelInvocation").and_then(Value::as_bool).unwrap_or(false);
        skills.push(Skill { name, file_path: skill_md.clone(), base_dir: dir.clone(), disable_model_invocation: disable });
    }
    skills
}

fn split_frontmatter(content: &str) -> (serde_json::Map<String, Value>, String) {
    let mut map = serde_json::Map::new();
    // Match Node's content.split(/\r?\n/) — strip CR before LF so CRLF files parse
    // identically to LF files.
    let lines: Vec<&str> = content.split("\r\n").flat_map(|s| s.split('\n')).collect();
    if lines.first().map(|l| l.trim() == "---").unwrap_or(false) {
        let mut end = None;
        for (i, line) in lines.iter().enumerate().skip(1) {
            if line.trim() == "---" {
                end = Some(i);
                break;
            }
        }
        if let Some(end) = end {
            let block: Vec<&str> = lines[1..end].to_vec();
            parse_yaml_frontmatter(&block, &mut map);
            let body = lines[end + 1..].join("\n");
            return (map, body);
        }
    }
    (map, content.to_string())
}

/// Minimal YAML frontmatter parser: `key: value` and `key: true|false`.
fn parse_yaml_frontmatter(lines: &[&str], map: &mut serde_json::Map<String, Value>) {
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else { continue };
        let key = key.trim().to_string();
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        if value == "true" {
            map.insert(key, Value::Bool(true));
        } else if value == "false" {
            map.insert(key, Value::Bool(false));
        } else {
            let unquoted = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(value);
            map.insert(key, Value::String(unquoted.to_string()));
        }
    }
}

pub fn strip_skill_frontmatter(content: &str) -> String {
    // Match Node's content.split(/\r?\n/) — strip CR before LF so CRLF files parse
    // identically to LF files.
    let lines: Vec<&str> = content.split("\r\n").flat_map(|s| s.split('\n')).collect();
    if lines.first().map(|l| l.trim() == "---").unwrap_or(false) {
        let mut end = None;
        for (i, line) in lines.iter().enumerate().skip(1) {
            if line.trim() == "---" {
                end = Some(i);
                break;
            }
        }
        // Node: lines.slice(end + 2) where end is relative to slice(1); in absolute
        // index that is (end_abs + 1). Match that — skip the closing "---" only.
        if let Some(end) = end {
            return lines[end + 1..].join("\n");
        }
    }
    content.to_string()
}

pub fn format_skills_for_prompt_compressed(skills: &[Skill]) -> String {
    let visible: Vec<&Skill> = skills.iter().filter(|s| !s.disable_model_invocation).collect();
    if visible.is_empty() {
        return String::new();
    }
    let mut by_root: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
    for skill in &visible {
        let skill_dir = skill.file_path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let root = skill_dir.parent().map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default();
        by_root.entry(root).or_default().push(skill.name.clone());
    }
    let mut lines = vec![
        "The following skills provide specialized instructions for specific tasks. When a skill name matches the task you are doing, read the SKILL.md at the listed location to load the full instructions. When a SKILL.md references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.".to_string(),
    ];
    for (root, mut names) in by_root {
        names.sort();
        lines.push(format!("Skills under {root}/<name>/SKILL.md:"));
        lines.push(names.iter().map(|n| format!("- {n}")).collect::<Vec<_>>().join("\n"));
    }
    lines.join("\n")
}

/// Approximation of pi-coding-agent `formatSkillsForPrompt` for the <4 skills case.
pub fn format_skills_for_prompt(skills: &[Skill]) -> String {
    let visible: Vec<&Skill> = skills.iter().filter(|s| !s.disable_model_invocation).collect();
    if visible.is_empty() {
        return String::new();
    }
    let mut lines = vec![
        "The following skills provide specialized instructions for specific tasks. When a skill name matches the task you are doing, read the SKILL.md at the listed location to load the full instructions.".to_string(),
    ];
    for skill in &visible {
        let location = skill.file_path.to_string_lossy().replace('\\', "/");
        lines.push(format!("- {}: {}", skill.name, location));
    }
    lines.join("\n")
}

pub fn format_alkaid_skills_prompt(skills: &[Skill]) -> String {
    let visible: Vec<&Skill> = skills.iter().filter(|s| !s.disable_model_invocation).collect();
    if visible.is_empty() {
        return String::new();
    }
    if visible.len() >= 4 {
        format_skills_for_prompt_compressed(skills)
    } else {
        format_skills_for_prompt(skills)
    }
}

/// Expand `/skill:<name> [args]` into the full skill body inline.
pub async fn expand_skill_command(text: &str, skills: &[Skill]) -> String {
    let re = regex::Regex::new(r"^/skill:([^\s]+)(?:\s+([\s\S]*))?$").unwrap();
    let Some(caps) = re.captures(text) else { return text.to_string() };
    let name = &caps[1];
    let args = caps.get(2).map(|m| m.as_str().trim().to_string()).unwrap_or_default();
    let Some(skill) = skills.iter().find(|s| s.name == name) else { return text.to_string() };
    match fs::read_to_string(&skill.file_path) {
        Ok(content) => {
            let body = strip_skill_frontmatter(&content).trim().to_string();
            let location = skill.file_path.to_string_lossy();
            let block = format!("<skill name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{}\n</skill>", skill.name, location, skill.base_dir.to_string_lossy(), body);
            if args.is_empty() { block } else { format!("{block}\n\n{args}") }
        }
        Err(_) => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_skill(root: &Path, name: &str, frontmatter: &str, body: &str) {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        let content = if frontmatter.is_empty() {
            body.to_string()
        } else {
            format!("---\n{frontmatter}\n---\n{body}")
        };
        fs::write(dir.join("SKILL.md"), content).unwrap();
    }

    #[test]
    fn split_frontmatter_extracts_yaml_block() {
        let content = "---\nname: my-skill\ndisableModelInvocation: true\n---\nbody text";
        let (map, body) = split_frontmatter(content);
        assert_eq!(map.get("name").and_then(Value::as_str), Some("my-skill"));
        assert_eq!(map.get("disableModelInvocation").and_then(Value::as_bool), Some(true));
        assert_eq!(body, "body text");
    }

    #[test]
    fn split_frontmatter_no_block_returns_whole_content() {
        let content = "just text\nno frontmatter";
        let (map, body) = split_frontmatter(content);
        assert!(map.is_empty());
        assert_eq!(body, content);
    }

    #[test]
    fn parse_yaml_frontmatter_quoted_value() {
        let mut map = serde_json::Map::new();
        let lines = vec!["name: \"quoted skill\""];
        parse_yaml_frontmatter(&lines, &mut map);
        assert_eq!(map.get("name").and_then(Value::as_str), Some("quoted skill"));
    }

    #[test]
    fn parse_yaml_frontmatter_skips_comments_and_blanks() {
        let mut map = serde_json::Map::new();
        let lines = vec!["# a comment", "", "key: value"];
        parse_yaml_frontmatter(&lines, &mut map);
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("key").and_then(Value::as_str), Some("value"));
    }

    #[test]
    fn strip_skill_frontmatter_removes_block() {
        let content = "---\nname: x\n---\n\nbody line 1\nbody line 2";
        let stripped = strip_skill_frontmatter(content);
        assert!(!stripped.contains("---"));
        assert!(stripped.contains("body line 1"));
    }

    #[test]
    fn strip_skill_frontmatter_no_block_unchanged() {
        let content = "no frontmatter here";
        assert_eq!(strip_skill_frontmatter(content), content);
    }

    #[test]
    fn load_skills_from_dir_scans_subdirs() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "alpha", "name: alpha", "do alpha things");
        write_skill(dir.path(), "beta", "name: beta\ndisableModelInvocation: true", "do beta things");
        // A directory without SKILL.md is ignored.
        fs::create_dir_all(dir.path().join("empty")).unwrap();
        let skills = load_skills_from_dir(dir.path());
        assert_eq!(skills.len(), 2);
        // Sorted by directory name.
        assert_eq!(skills[0].name, "alpha");
        assert_eq!(skills[1].name, "beta");
        assert!(!skills[0].disable_model_invocation);
        assert!(skills[1].disable_model_invocation);
    }

    #[test]
    fn load_skills_falls_back_to_dir_name_without_name_field() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "gamma", "", "body");
        let skills = load_skills_from_dir(dir.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "gamma");
    }

    #[test]
    fn format_skills_for_prompt_lists_visible_skills() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "alpha", "name: alpha", "body");
        let skills = load_skills_from_dir(dir.path());
        let prompt = format_skills_for_prompt(&skills);
        assert!(prompt.contains("alpha"));
        assert!(prompt.contains("SKILL.md"));
    }

    #[test]
    fn format_skills_hides_disabled_model_invocation() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "alpha", "name: alpha\ndisableModelInvocation: true", "body");
        let skills = load_skills_from_dir(dir.path());
        let prompt = format_skills_for_prompt(&skills);
        // Disabled skills are filtered out of the visible prompt.
        assert!(!prompt.contains("alpha"));
    }

    #[test]
    fn format_alkaid_skills_prompt_uses_compressed_when_four_or_more() {
        let dir = tempdir().unwrap();
        for name in ["a", "b", "c", "d"] {
            write_skill(dir.path(), name, &format!("name: {name}"), "body");
        }
        let skills = load_skills_from_dir(dir.path());
        let prompt = format_alkaid_skills_prompt(&skills);
        // Compressed format groups by root and lists names under a header.
        assert!(prompt.contains("Skills under"));
    }

    #[test]
    fn format_alkaid_skills_prompt_uses_full_when_under_four() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "alpha", "name: alpha", "body");
        let skills = load_skills_from_dir(dir.path());
        let prompt = format_alkaid_skills_prompt(&skills);
        assert!(prompt.contains("- alpha:"));
    }

    #[tokio::test]
    async fn expand_skill_command_inlines_body() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "alpha", "name: alpha", "do the thing");
        let skills = load_skills_from_dir(dir.path());
        let expanded = expand_skill_command("/skill:alpha", &skills).await;
        assert!(expanded.contains("<skill name=\"alpha\""));
        assert!(expanded.contains("do the thing"));
        assert!(expanded.contains("</skill>"));
    }

    #[tokio::test]
    async fn expand_skill_command_appends_args() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "alpha", "name: alpha", "do the thing");
        let skills = load_skills_from_dir(dir.path());
        let expanded = expand_skill_command("/skill:alpha extra context", &skills).await;
        assert!(expanded.contains("do the thing"));
        assert!(expanded.contains("extra context"));
    }

    #[tokio::test]
    async fn expand_skill_command_unknown_skill_returns_original() {
        let dir = tempdir().unwrap();
        let skills = load_skills_from_dir(dir.path());
        let expanded = expand_skill_command("/skill:nonexistent", &skills).await;
        assert_eq!(expanded, "/skill:nonexistent");
    }

    #[tokio::test]
    async fn expand_skill_command_non_skill_text_unchanged() {
        let dir = tempdir().unwrap();
        let skills = load_skills_from_dir(dir.path());
        let expanded = expand_skill_command("just a normal prompt", &skills).await;
        assert_eq!(expanded, "just a normal prompt");
    }
}
