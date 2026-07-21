//! 统一 Agent 全局指令：内容集中存放在 `~/.nova/global-agent-instructions.md`，
//! 再按各后端的原生用户级入口生成托管配置。

use crate::threads::AgentKind;
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

const CENTRAL_FILE: &str = "global-agent-instructions.md";
const BLOCK_START: &str = "<!-- NOVA_GLOBAL_INSTRUCTIONS_START -->";
const BLOCK_END: &str = "<!-- NOVA_GLOBAL_INSTRUCTIONS_END -->";
const CURSOR_MARKER: &str = "<!-- NOVA_GLOBAL_INSTRUCTIONS_CURSOR -->";

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AgentInstructionTarget {
    pub agent_kind: String,
    pub label: String,
    pub path: String,
    pub status: String,
    pub detail: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct GlobalAgentInstructions {
    pub content: String,
    pub path: String,
    pub targets: Vec<AgentInstructionTarget>,
}

#[derive(Clone, Copy)]
enum TargetFormat {
    Markdown,
    CursorRule,
}

struct Target {
    kind: AgentKind,
    label: &'static str,
    path: PathBuf,
    format: TargetFormat,
}

pub fn get_global_instructions(config_dir: &Path) -> GlobalAgentInstructions {
    let central = central_path(config_dir);
    let content = fs::read_to_string(&central).unwrap_or_default();
    let active = !content.trim().is_empty();
    let targets = normal_targets()
        .unwrap_or_default()
        .iter()
        .map(|target| inspect_target(target, active))
        .collect();
    GlobalAgentInstructions {
        content,
        path: central.to_string_lossy().to_string(),
        targets,
    }
}

pub fn set_global_instructions(
    config_dir: &Path,
    content: &str,
) -> Result<GlobalAgentInstructions, String> {
    if content.contains(BLOCK_START)
        || content.contains(BLOCK_END)
        || content.contains(CURSOR_MARKER)
    {
        return Err("全局指令不能包含 Nova 内部托管标记".into());
    }
    fs::create_dir_all(config_dir).map_err(|e| format!("创建 Nova 配置目录失败：{e}"))?;
    let central = central_path(config_dir);
    let active = !content.trim().is_empty();
    if active {
        fs::write(&central, content).map_err(|e| format!("保存全局指令失败：{e}"))?;
    }
    let targets = if cfg!(debug_assertions) {
        Vec::new()
    } else {
        normal_targets()?
            .iter()
            .map(|target| sync_target(target, if active { content } else { "" }))
            .collect()
    };
    if !active {
        let _ = fs::remove_file(&central);
    }
    Ok(GlobalAgentInstructions {
        content: if active {
            content.to_string()
        } else {
            String::new()
        },
        path: central.to_string_lossy().to_string(),
        targets,
    })
}

/// 启动时重新同步一次，修复用户移动/删除了后端配置入口的情况。
pub fn sync_global_instructions(config_dir: &Path) -> Result<(), String> {
    if cfg!(debug_assertions) {
        return Ok(());
    }
    let content = fs::read_to_string(central_path(config_dir)).unwrap_or_default();
    for target in normal_targets()? {
        let _ = sync_target(&target, &content);
    }
    Ok(())
}

/// 额度漫游会为每个后端准备隔离 HOME/config；把本机全局指令同步进该真实运行环境。
pub fn sync_backend_with_env(
    config_dir: &Path,
    kind: &AgentKind,
    env: &HashMap<String, String>,
) -> Result<(), String> {
    if cfg!(debug_assertions) {
        return Ok(());
    }
    let content = fs::read_to_string(central_path(config_dir)).unwrap_or_default();
    if content.trim().is_empty() {
        return Ok(());
    }
    let target = target_for(kind, env)?;
    let _ = sync_target(&target, &content);
    Ok(())
}

fn central_path(config_dir: &Path) -> PathBuf {
    config_dir.join(CENTRAL_FILE)
}

fn normal_targets() -> Result<Vec<Target>, String> {
    let env = HashMap::new();
    [
        AgentKind::Alkaid,
        AgentKind::Devin,
        AgentKind::Codex,
        AgentKind::CodeBuddy,
        AgentKind::ClaudeCode,
        AgentKind::Cursor,
        AgentKind::OpenCode,
    ]
    .iter()
    .map(|kind| target_for(kind, &env))
    .collect()
}

fn target_for(kind: &AgentKind, overrides: &HashMap<String, String>) -> Result<Target, String> {
    let home = configured_dir(overrides, "USERPROFILE")
        .or_else(|| configured_dir(overrides, "HOME"))
        .or_else(user_home_dir)
        .ok_or("无法确定用户主目录")?;
    let (label, path, format) = match kind {
        AgentKind::Alkaid => (
            "Alkaid",
            home.join(".nova").join("AGENTS.md"),
            TargetFormat::Markdown,
        ),
        AgentKind::Devin => {
            #[cfg(windows)]
            let root = configured_dir(overrides, "APPDATA")
                .unwrap_or_else(|| home.join("AppData").join("Roaming"));
            #[cfg(not(windows))]
            let root = configured_dir(overrides, "APPDATA")
                .or_else(|| configured_dir(overrides, "XDG_CONFIG_HOME"))
                .unwrap_or_else(|| home.join(".config"));
            (
                "Devin",
                root.join("devin").join("AGENTS.md"),
                TargetFormat::Markdown,
            )
        }
        AgentKind::Codex | AgentKind::CodexPlus => {
            let root =
                configured_dir(overrides, "CODEX_HOME").unwrap_or_else(|| home.join(".codex"));
            (kind.label(), root.join("AGENTS.md"), TargetFormat::Markdown)
        }
        AgentKind::CodeBuddy | AgentKind::CodeBuddyPlus => {
            let root = configured_dir(overrides, "CODEBUDDY_CONFIG_DIR")
                .unwrap_or_else(|| home.join(".codebuddy"));
            (
                kind.label(),
                root.join("CODEBUDDY.md"),
                TargetFormat::Markdown,
            )
        }
        AgentKind::ClaudeCode => {
            let root = configured_dir(overrides, "CLAUDE_CONFIG_DIR")
                .unwrap_or_else(|| home.join(".claude"));
            (
                "Claude Code",
                root.join("CLAUDE.md"),
                TargetFormat::Markdown,
            )
        }
        AgentKind::Cursor => {
            let root = configured_dir(overrides, "CURSOR_CONFIG_DIR")
                .unwrap_or_else(|| home.join(".cursor"));
            (
                "Cursor",
                root.join("rules").join("nova-global.mdc"),
                TargetFormat::CursorRule,
            )
        }
        AgentKind::OpenCode | AgentKind::OpenCodePlus => {
            let root = configured_dir(overrides, "XDG_CONFIG_HOME")
                .unwrap_or_else(|| home.join(".config"));
            (
                kind.label(),
                root.join("opencode").join("AGENTS.md"),
                TargetFormat::Markdown,
            )
        }
    };
    Ok(Target {
        kind: kind.clone(),
        label,
        path,
        format,
    })
}

fn configured_dir(overrides: &HashMap<String, String>, name: &str) -> Option<PathBuf> {
    overrides
        .get(name)
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os(name)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
}

fn user_home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

fn sync_target(target: &Target, content: &str) -> AgentInstructionTarget {
    let result = match target.format {
        TargetFormat::Markdown => sync_markdown(&target.path, content),
        TargetFormat::CursorRule => sync_cursor_rule(&target.path, content),
    };
    match result {
        Ok((status, detail)) => target_status(target, status, detail),
        Err(error) => target_status(target, "error", error.to_string()),
    }
}

fn inspect_target(target: &Target, active: bool) -> AgentInstructionTarget {
    if !active {
        return target_status(target, "inactive", "未启用".into());
    }
    if is_symlink(&target.path) {
        return target_status(target, "conflict", "目标是符号链接，未覆盖".into());
    }
    if !target.path.exists() {
        return target_status(target, "pending", "保存时创建原生配置入口".into());
    }
    let Ok(existing) = fs::read_to_string(&target.path) else {
        return target_status(target, "conflict", "现有文件不是可合并的 UTF-8 文本".into());
    };
    let managed = match target.format {
        TargetFormat::Markdown => existing.contains(BLOCK_START) && existing.contains(BLOCK_END),
        TargetFormat::CursorRule => existing.contains(CURSOR_MARKER),
    };
    if managed {
        target_status(target, "managed", "已由 Nova 托管".into())
    } else if matches!(target.format, TargetFormat::Markdown) {
        target_status(target, "pending", "保存时保留原内容并合并 Nova 指令".into())
    } else {
        target_status(target, "conflict", "同名 Cursor Rule 已存在，未覆盖".into())
    }
}

fn target_status(
    target: &Target,
    status: impl Into<String>,
    detail: String,
) -> AgentInstructionTarget {
    AgentInstructionTarget {
        agent_kind: target.kind.as_str().to_string(),
        label: target.label.to_string(),
        path: target.path.to_string_lossy().to_string(),
        status: status.into(),
        detail,
    }
}

fn sync_markdown(path: &Path, content: &str) -> Result<(&'static str, String), String> {
    if is_symlink(path) {
        return Ok(("conflict", "目标是符号链接，未覆盖".into()));
    }
    let existing = if path.exists() {
        fs::read_to_string(path).map_err(|_| "现有文件不是可合并的 UTF-8 文本")?
    } else {
        String::new()
    };
    if content.trim().is_empty() {
        let (next, removed) = remove_managed_block(&existing);
        if !removed {
            return Ok(("inactive", "未修改现有配置".into()));
        }
        if next.trim().is_empty() {
            fs::remove_file(path).map_err(|e| e.to_string())?;
        } else {
            fs::write(path, next).map_err(|e| e.to_string())?;
        }
        return Ok(("inactive", "已移除 Nova 托管区块".into()));
    }
    ensure_parent(path)?;
    let had_content = !existing.trim().is_empty();
    let had_block = existing.contains(BLOCK_START) && existing.contains(BLOCK_END);
    fs::write(path, upsert_managed_block(&existing, content)).map_err(|e| e.to_string())?;
    Ok((
        if had_content { "merged" } else { "managed" },
        if had_block {
            "已更新 Nova 托管区块".into()
        } else if had_content {
            "已保留原内容并合并 Nova 指令".into()
        } else {
            "已创建后端原生配置入口".into()
        },
    ))
}

fn sync_cursor_rule(path: &Path, content: &str) -> Result<(&'static str, String), String> {
    if is_symlink(path) {
        return Ok(("conflict", "目标是符号链接，未覆盖".into()));
    }
    if content.trim().is_empty() {
        if !path.exists() {
            return Ok(("inactive", "未启用".into()));
        }
        let existing = fs::read_to_string(path).unwrap_or_default();
        if existing.contains(CURSOR_MARKER) {
            fs::remove_file(path).map_err(|e| e.to_string())?;
            return Ok(("inactive", "已移除 Nova Cursor Rule".into()));
        }
        return Ok(("inactive", "未修改现有配置".into()));
    }
    if path.exists() {
        let existing =
            fs::read_to_string(path).map_err(|_| "现有 Cursor Rule 不是可管理的 UTF-8 文本")?;
        if !existing.contains(CURSOR_MARKER) {
            return Ok(("conflict", "同名 Cursor Rule 已存在，未覆盖".into()));
        }
    }
    ensure_parent(path)?;
    fs::write(path, cursor_rule_content(content)).map_err(|e| e.to_string())?;
    Ok(("managed", "已生成 alwaysApply 全局 Rule".into()))
}

fn managed_block(content: &str) -> String {
    format!("{BLOCK_START}\n{}\n{BLOCK_END}\n", content.trim_end())
}

fn upsert_managed_block(existing: &str, content: &str) -> String {
    let (base, _) = remove_managed_block(existing);
    if base.trim().is_empty() {
        managed_block(content)
    } else {
        format!("{}\n\n{}", base.trim_end(), managed_block(content))
    }
}

fn remove_managed_block(existing: &str) -> (String, bool) {
    let Some(start) = existing.find(BLOCK_START) else {
        return (existing.to_string(), false);
    };
    let after_start = start + BLOCK_START.len();
    let Some(end_rel) = existing[after_start..].find(BLOCK_END) else {
        return (existing.to_string(), false);
    };
    let end = after_start + end_rel + BLOCK_END.len();
    let next = format!("{}{}", &existing[..start], &existing[end..]);
    let trimmed = next.trim_end();
    if trimmed.is_empty() {
        (String::new(), true)
    } else {
        (format!("{trimmed}\n"), true)
    }
}

fn cursor_rule_content(content: &str) -> String {
    format!(
        "---\ndescription: Nova global agent instructions\nalwaysApply: true\n---\n{CURSOR_MARKER}\n{}\n",
        content.trim_end()
    )
}

fn ensure_parent(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_block_preserves_existing_content_and_updates_in_place() {
        let original = "# Existing\n\nKeep me.\n";
        let first = upsert_managed_block(original, "Use Chinese.");
        let second = upsert_managed_block(&first, "Use tests.");
        assert!(second.contains("Keep me."));
        assert!(second.contains("Use tests."));
        assert!(!second.contains("Use Chinese."));
        assert_eq!(second.matches(BLOCK_START).count(), 1);
    }

    #[test]
    fn removing_managed_block_keeps_user_content() {
        let merged = upsert_managed_block("# Existing\n", "Nova rule");
        let (cleaned, removed) = remove_managed_block(&merged);
        assert!(removed);
        assert_eq!(cleaned, "# Existing\n");
    }

    #[test]
    fn cursor_adapter_is_always_apply() {
        let rule = cursor_rule_content("Use Chinese.");
        assert!(rule.contains("alwaysApply: true"));
        assert!(rule.contains(CURSOR_MARKER));
    }
}
