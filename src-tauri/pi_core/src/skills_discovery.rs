//! Skill discovery from disk, ported from `pi-coding-agent`
//! (`dist/core/skills.js` `loadSkillsFromDir`) and `alkaid-core.mjs`
//! (`expandAlkaidSkillCommand`, `stripSkillFrontmatter`).
//!
//! Discovery rules (matching pi-coding-agent):
//! - if a directory contains a `SKILL.md` file, it is a skill root: load that
//!   one skill and do **not** recurse further;
//! - otherwise recurse into subdirectories, and (only at the top-level root)
//!   also load direct `*.md` children;
//! - skip dot-entries and `node_modules`.
//!
//! Parity boundaries (documented):
//! - Frontmatter parsing is a minimal `key: value` subset of YAML (top-level
//!   scalar keys, optional surrounding quotes). Block scalars / nested values
//!   are not supported; these are rare in `SKILL.md` frontmatter.
//! - pi-coding-agent applies a gitignore-style `ignore` matcher; skill roots
//!   rarely ship a `.gitignore`, so it is not reproduced here.
//! - Discovery order is sorted by entry name for determinism (node `readdir`
//!   order is OS-dependent); the compressed prompt sorts names anyway.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::paths::dirname;
use crate::skills::Skill;

/// Strip matching surrounding quotes and trim (port of `clean_frontmatter_value`).
fn clean_value(value: &str) -> String {
    let trimmed = value.trim();
    trimmed
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .or_else(|| trimmed.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
        .unwrap_or(trimmed)
        .trim()
        .to_string()
}

/// Minimal frontmatter parser: extract the leading `---\n...\n---` block and
/// read top-level `key: value` pairs. Mirrors the JS `extractFrontmatter`
/// boundary (`indexOf("\n---", 3)`, `slice(4, endIndex)`).
pub fn parse_frontmatter(content: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    if !normalized.starts_with("---") {
        return map;
    }
    let Some(end) = normalized[3..].find("\n---").map(|i| i + 3) else {
        return map;
    };
    let yaml = if end >= 4 { &normalized[4..end] } else { "" };
    for line in yaml.split('\n') {
        if line.starts_with(' ') || line.starts_with('\t') {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_string();
            if key.is_empty() {
                continue;
            }
            map.insert(key, clean_value(value));
        }
    }
    map
}

fn load_skill_from_file(path: &Path) -> Option<Skill> {
    let content = fs::read_to_string(path).ok()?;
    let frontmatter = parse_frontmatter(&content);
    let skill_dir = path.parent()?;
    let parent_name = skill_dir
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "skill".to_string());
    let name = frontmatter
        .get("name")
        .filter(|name| !name.is_empty())
        .cloned()
        .unwrap_or(parent_name);
    let description = frontmatter.get("description").cloned().unwrap_or_default();
    if description.trim().is_empty() {
        return None;
    }
    let disable_model_invocation = frontmatter
        .get("disable-model-invocation")
        .map(|value| value == "true")
        .unwrap_or(false);
    Some(Skill {
        name,
        description,
        file_path: path.to_string_lossy().to_string(),
        disable_model_invocation,
    })
}

fn load_skills_internal(dir: &Path, include_root_files: bool) -> Vec<Skill> {
    let mut skills = Vec::new();
    let Ok(read_dir) = fs::read_dir(dir) else {
        return skills;
    };
    let mut entries: Vec<_> = read_dir.filter_map(|entry| entry.ok()).collect();
    entries.sort_by_key(|entry| entry.file_name());

    // A directory containing SKILL.md is a skill root: load it, stop recursing.
    for entry in &entries {
        if entry.file_name() != "SKILL.md" {
            continue;
        }
        let full = entry.path();
        if !full.is_file() {
            continue;
        }
        if let Some(skill) = load_skill_from_file(&full) {
            skills.push(skill);
        }
        return skills;
    }

    for entry in &entries {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let full = entry.path();
        if full.is_dir() {
            skills.extend(load_skills_internal(&full, false));
            continue;
        }
        if !full.is_file() || !include_root_files || !name.ends_with(".md") {
            continue;
        }
        if let Some(skill) = load_skill_from_file(&full) {
            skills.push(skill);
        }
    }
    skills
}

/// Port of `loadSkillsFromDir({ dir, source: "user" })`.
pub fn load_skills_from_dir(dir: &Path) -> Vec<Skill> {
    load_skills_internal(dir, true)
}

/// Port of alkaid `stripSkillFrontmatter`.
pub fn strip_skill_frontmatter(content: &str) -> String {
    if !content.starts_with("---") {
        return content.to_string();
    }
    let normalized = content.replace("\r\n", "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();
    if lines.first().map(|line| line.trim()) != Some("---") {
        return content.to_string();
    }
    match lines[1..].iter().position(|line| line.trim() == "---") {
        Some(index) => lines[index + 2..].join("\n"),
        None => content.to_string(),
    }
}

/// Port of alkaid `expandAlkaidSkillCommand`: expand a leading `/skill:<name>`
/// invocation into an inline `<skill>` block read from the skill file.
pub fn expand_skill_command(text: &str, skills: &[Skill]) -> String {
    let re = regex::Regex::new(r"^/skill:([^\s]+)(?:\s+([\s\S]*))?$").unwrap();
    let Some(captures) = re.captures(text) else {
        return text.to_string();
    };
    let name = captures.get(1).map(|m| m.as_str()).unwrap_or("");
    let Some(skill) = skills.iter().find(|candidate| candidate.name == name) else {
        return text.to_string();
    };
    let Ok(raw) = fs::read_to_string(&skill.file_path) else {
        return text.to_string();
    };
    let body = strip_skill_frontmatter(&raw).trim().to_string();
    let base_dir = dirname(&skill.file_path);
    let block = format!(
        "<skill name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{}\n</skill>",
        skill.name, skill.file_path, base_dir, body
    );
    let args = captures.get(2).map(|m| m.as_str()).unwrap_or("").trim();
    if args.is_empty() {
        block
    } else {
        format!("{block}\n\n{args}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parses_frontmatter_fields() {
        let content = "---\nname: demo\ndescription: \"A demo skill\"\ndisable-model-invocation: true\n---\nbody";
        let fm = parse_frontmatter(content);
        assert_eq!(fm.get("name").map(String::as_str), Some("demo"));
        assert_eq!(fm.get("description").map(String::as_str), Some("A demo skill"));
        assert_eq!(fm.get("disable-model-invocation").map(String::as_str), Some("true"));
    }

    #[test]
    fn frontmatter_falls_back_to_parent_name_and_requires_description() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("my-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        // No name → parent dir name; has description → loaded.
        fs::write(skill_dir.join("SKILL.md"), "---\ndescription: does things\n---\n").unwrap();
        // No description → skipped.
        let no_desc = dir.path().join("nodesc");
        fs::create_dir_all(&no_desc).unwrap();
        fs::write(no_desc.join("SKILL.md"), "---\nname: x\n---\n").unwrap();

        let skills = load_skills_from_dir(dir.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "my-skill");
        assert_eq!(skills[0].description, "does things");
        assert!(!skills[0].disable_model_invocation);
    }

    #[test]
    fn skill_md_stops_recursion_and_root_md_loaded() {
        let dir = tempfile::tempdir().unwrap();
        // A nested skill root: SKILL.md present → its sub.md is NOT loaded.
        let nested = dir.path().join("nested");
        fs::create_dir_all(nested.join("inner")).unwrap();
        fs::write(nested.join("SKILL.md"), "---\nname: nested-skill\ndescription: n\n---\n").unwrap();
        fs::write(nested.join("inner").join("SKILL.md"), "---\nname: inner\ndescription: i\n---\n").unwrap();
        // A direct root .md file (loaded because include_root_files at top).
        fs::write(dir.path().join("root.md"), "---\nname: root-skill\ndescription: r\n---\n").unwrap();
        // node_modules is skipped.
        let nm = dir.path().join("node_modules").join("pkg");
        fs::create_dir_all(&nm).unwrap();
        fs::write(nm.join("SKILL.md"), "---\nname: skipped\ndescription: s\n---\n").unwrap();

        let mut skills = load_skills_from_dir(dir.path());
        skills.sort_by(|a, b| a.name.cmp(&b.name));
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        // nested-skill (recursion stopped, inner not loaded), root-skill; node_modules skipped.
        assert_eq!(names, vec!["nested-skill", "root-skill"]);
    }

    #[test]
    fn strips_frontmatter() {
        assert_eq!(strip_skill_frontmatter("---\nname: x\n---\nbody here"), "body here");
        assert_eq!(strip_skill_frontmatter("no frontmatter"), "no frontmatter");
        assert_eq!(strip_skill_frontmatter("---\nname: x\n"), "---\nname: x\n");
    }

    #[test]
    fn expands_skill_command() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("deploy");
        fs::create_dir_all(&skill_dir).unwrap();
        let path = skill_dir.join("SKILL.md");
        fs::write(&path, "---\nname: deploy\ndescription: d\n---\nRun the deploy.").unwrap();
        let skills = load_skills_from_dir(dir.path());

        let expanded = expand_skill_command("/skill:deploy now", &skills);
        assert!(expanded.contains("<skill name=\"deploy\""));
        assert!(expanded.contains("Run the deploy."));
        assert!(expanded.ends_with("\n\nnow"));

        // Unknown skill → unchanged.
        assert_eq!(expand_skill_command("/skill:nope", &skills), "/skill:nope");
        // Not a skill command → unchanged.
        assert_eq!(expand_skill_command("hello", &skills), "hello");
    }
}
