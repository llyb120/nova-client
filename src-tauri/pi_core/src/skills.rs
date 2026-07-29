//! Skill prompt formatting, ported from `alkaid-core.mjs`
//! (`formatAlkaidSkillsPrompt`, `formatSkillsForPromptCompressed`) and
//! `pi-coding-agent/dist/core/skills.js` (`formatSkillsForPrompt`).
//!
//! Root/name sorting uses byte order (Rust `Ord`/`BTreeMap`), matching JS
//! `Array.sort` for ASCII; non-ASCII skill names or roots may order differently
//! (JS sorts by UTF-16 code units).

use std::collections::BTreeMap;

use crate::paths::dirname;

/// Visible-skill count at which alkaid switches from the full XML block to the
/// compressed by-root listing.
const SKILL_COMPRESSION_MIN_COUNT: usize = 4;

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub file_path: String,
    pub disable_model_invocation: bool,
}

/// Port of `escapeXml`: `&` is replaced first to avoid double-escaping.
fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Port of pi-coding-agent `formatSkillsForPrompt`: the full XML block.
pub fn format_skills_for_prompt(skills: &[Skill]) -> String {
    let visible: Vec<&Skill> = skills.iter().filter(|s| !s.disable_model_invocation).collect();
    if visible.is_empty() {
        return String::new();
    }
    let mut lines: Vec<String> = vec![
        "\n\nThe following skills provide specialized instructions for specific tasks.".to_string(),
        "Use the read tool to load a skill's file when the task matches its description.".to_string(),
        "When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.".to_string(),
        String::new(),
        "<available_skills>".to_string(),
    ];
    for skill in &visible {
        lines.push("  <skill>".to_string());
        lines.push(format!("    <name>{}</name>", escape_xml(&skill.name)));
        lines.push(format!(
            "    <description>{}</description>",
            escape_xml(&skill.description)
        ));
        lines.push(format!(
            "    <location>{}</location>",
            escape_xml(&skill.file_path)
        ));
        lines.push("  </skill>".to_string());
    }
    lines.push("</available_skills>".to_string());
    lines.join("\n")
}

/// Port of alkaid `formatSkillsForPromptCompressed`: group visible skill names
/// by `dirname(dirname(filePath))`, with sorted roots and sorted names.
pub fn format_skills_for_prompt_compressed(skills: &[Skill]) -> String {
    let visible: Vec<&Skill> = skills.iter().filter(|s| !s.disable_model_invocation).collect();
    if visible.is_empty() {
        return String::new();
    }
    let mut by_root: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for skill in &visible {
        let skill_dir = dirname(&skill.file_path);
        let root = dirname(&skill_dir).replace('\\', "/");
        by_root.entry(root).or_default().push(skill.name.clone());
    }
    let mut lines: Vec<String> = vec![
        "The following skills provide specialized instructions for specific tasks. When a skill name matches the task you are doing, read the SKILL.md at the listed location to load the full instructions. When a SKILL.md references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.".to_string(),
    ];
    for (root, mut names) in by_root {
        names.sort();
        lines.push(format!("Skills under {root}/<name>/SKILL.md:"));
        lines.push(
            names
                .iter()
                .map(|name| format!("- {name}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    lines.join("\n")
}

/// Port of alkaid `formatAlkaidSkillsPrompt`: empty when no visible skills,
/// compressed listing at `>= 4` visible skills, otherwise the trimmed XML block.
pub fn format_alkaid_skills_prompt(skills: &[Skill]) -> String {
    let visible_count = skills.iter().filter(|s| !s.disable_model_invocation).count();
    if visible_count == 0 {
        return String::new();
    }
    if visible_count >= SKILL_COMPRESSION_MIN_COUNT {
        return format_skills_for_prompt_compressed(skills);
    }
    format_skills_for_prompt(skills).trim().to_string()
}
