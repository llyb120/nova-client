//! 统一 Skill 管理：集中存放在 `~/.nova/skills/`，启动后端时用软链接/目录联接
//! 同步到各 agent 的全局 skills 目录（不拷贝，保持一处修改处处生效）。

use serde::Serialize;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub path: String,
}

pub fn skills_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("skills")
}

pub fn ensure_skills_dir(config_dir: &Path) -> PathBuf {
    let dir = skills_dir(config_dir);
    let _ = fs::create_dir_all(&dir);
    dir
}

fn user_home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .or_else(|| {
            let drive = std::env::var_os("HOMEDRIVE")?;
            let path = std::env::var_os("HOMEPATH")?;
            Some(PathBuf::from(format!(
                "{}{}",
                drive.to_string_lossy(),
                path.to_string_lossy()
            )))
        })
}

fn clean_frontmatter_value(value: &str) -> String {
    let trimmed = value.trim();
    trimmed
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .or_else(|| {
            trimmed
                .strip_prefix('\'')
                .and_then(|v| v.strip_suffix('\''))
        })
        .unwrap_or(trimmed)
        .trim()
        .to_string()
}

fn frontmatter_value(contents: &str, key: &str) -> Option<String> {
    let mut lines = contents.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    for line in lines {
        let line = line.trim_end();
        if line.trim() == "---" {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            if k.trim() == key {
                let cleaned = clean_frontmatter_value(v);
                if !cleaned.is_empty() {
                    return Some(cleaned);
                }
            }
        }
    }
    None
}

fn read_skill_meta(skill_md: &Path) -> (String, String) {
    let fallback = skill_md
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "skill".into());
    let Ok(contents) = fs::read_to_string(skill_md) else {
        return (fallback, String::new());
    };
    let name = frontmatter_value(&contents, "name").unwrap_or(fallback);
    let description = frontmatter_value(&contents, "description").unwrap_or_default();
    (name, description)
}

fn is_skill_dir(dir: &Path) -> bool {
    dir.is_dir() && dir.join("SKILL.md").is_file()
}

/// 各后端用户级 skills 根目录（全局）。
pub fn backend_skill_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = std::env::var_os("CODEX_HOME").map(PathBuf::from) {
        roots.push(home.join("skills"));
    }
    if let Some(home) = user_home_dir() {
        roots.push(home.join(".codex").join("skills"));
        roots.push(home.join(".claude").join("skills"));
        roots.push(home.join(".cursor").join("skills"));
        roots.push(home.join(".agents").join("skills"));
        roots.push(home.join(".config").join("opencode").join("skills"));
    }
    roots.sort();
    roots.dedup();
    roots
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => {
            let na = a.to_string_lossy().replace('\\', "/").to_lowercase();
            let nb = b.to_string_lossy().replace('\\', "/").to_lowercase();
            na == nb
        }
    }
}

fn resolve_link_target(link: &Path) -> Option<PathBuf> {
    let target = fs::read_link(link).ok()?;
    if target.is_absolute() {
        Some(target)
    } else {
        Some(link.parent()?.join(target))
    }
}

fn is_managed_link(path: &Path, expected: &Path) -> bool {
    match resolve_link_target(path) {
        Some(target) => paths_equal(&target, expected),
        None => false,
    }
}

fn is_any_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

#[cfg(unix)]
fn link_dir(original: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(original, link)
}

#[cfg(windows)]
fn link_dir(original: &Path, link: &Path) -> io::Result<()> {
    // 优先目录符号链接（开发者模式 / 提权可用）；失败则退回目录联接（无需管理员）。
    if std::os::windows::fs::symlink_dir(original, link).is_ok() {
        return Ok(());
    }
    use std::os::windows::process::CommandExt;
    let status = std::process::Command::new("cmd")
        .args([
            "/C",
            "mklink",
            "/J",
            &link.to_string_lossy(),
            &original.to_string_lossy(),
        ])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Other,
            format!("mklink /J 失败（exit {:?})", status.code()),
        ))
    }
}

fn remove_path_quiet(path: &Path) {
    if !path.exists() && !is_any_symlink(path) {
        return;
    }
    if path.is_dir() && !is_any_symlink(path) {
        let _ = fs::remove_dir_all(path);
    } else {
        let _ = fs::remove_dir(path);
        let _ = fs::remove_file(path);
    }
}

fn sanitize_skill_name(name: &str) -> String {
    let trimmed = name.trim();
    let mut out = String::new();
    for ch in trimmed.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
            out.push(ch);
        } else if ch == ' ' {
            out.push('-');
        }
    }
    let out = out.trim_matches('.').trim_matches('-').to_string();
    if out.is_empty() {
        "skill".into()
    } else {
        out
    }
}

fn copy_dir_all(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// 把借入方本机的 Nova skills 复制到额度租借的隔离后端目录。
/// 使用会话快照而不是链接，避免隔离进程修改用户的原始 skill。
pub fn copy_skills_to_runtime(config_dir: &Path, destination: &Path) -> Result<(), String> {
    let source = skills_dir(config_dir);
    if !source.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(destination).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(source).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if !is_skill_dir(&path) {
            continue;
        }
        copy_dir_all(&path, &destination.join(entry.file_name())).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn find_skill_md(dir: &Path, depth: usize) -> Option<PathBuf> {
    if depth == 0 || !dir.is_dir() {
        return None;
    }
    let skill = dir.join("SKILL.md");
    if skill.is_file() {
        return Some(skill);
    }
    let mut entries: Vec<_> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    entries.sort();
    for path in entries {
        if let Some(found) = find_skill_md(&path, depth - 1) {
            return Some(found);
        }
    }
    None
}

fn install_from_skill_root(config_dir: &Path, skill_root: &Path) -> Result<SkillInfo, String> {
    let skill_md = skill_root.join("SKILL.md");
    if !skill_md.is_file() {
        return Err("目录中没有 SKILL.md".into());
    }
    let (meta_name, description) = read_skill_meta(&skill_md);
    let folder_name = skill_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| meta_name.clone());
    let name = sanitize_skill_name(if meta_name.is_empty() {
        &folder_name
    } else {
        &meta_name
    });
    let dest = ensure_skills_dir(config_dir).join(&name);
    if paths_equal(skill_root, &dest) {
        return Ok(SkillInfo {
            name: name.clone(),
            description,
            path: dest.to_string_lossy().to_string(),
        });
    }
    if dest.exists() || is_any_symlink(&dest) {
        remove_path_quiet(&dest);
    }
    copy_dir_all(skill_root, &dest).map_err(|e| format!("安装 skill 失败：{e}"))?;
    let _ = sync_skills_to_backends(config_dir);
    let (_, description) = read_skill_meta(&dest.join("SKILL.md"));
    Ok(SkillInfo {
        name,
        description,
        path: dest.to_string_lossy().to_string(),
    })
}

pub fn list_skills(config_dir: &Path) -> Vec<SkillInfo> {
    let root = ensure_skills_dir(config_dir);
    let mut skills = Vec::new();
    let Ok(entries) = fs::read_dir(&root) else {
        return skills;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !is_skill_dir(&path) {
            continue;
        }
        let (meta_name, description) = read_skill_meta(&path.join("SKILL.md"));
        let folder = entry.file_name().to_string_lossy().to_string();
        skills.push(SkillInfo {
            // 以目录名为准（删除/同步的身份）；frontmatter name 仅作展示补充
            name: folder.clone(),
            description: if description.is_empty() {
                if meta_name.is_empty() || meta_name == folder {
                    String::new()
                } else {
                    meta_name
                }
            } else {
                description
            },
            path: path.to_string_lossy().to_string(),
        });
    }
    skills.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    skills
}

pub fn install_skill_path(config_dir: &Path, path: &Path) -> Result<SkillInfo, String> {
    if !path.exists() {
        return Err(format!("路径不存在：{}", path.to_string_lossy()));
    }
    if path.is_file() {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if ext != "zip" {
            return Err("仅支持 zip 压缩包或包含 SKILL.md 的文件夹".into());
        }
        return install_skill_zip(config_dir, path);
    }
    if is_skill_dir(path) {
        return install_from_skill_root(config_dir, path);
    }
    if let Some(skill_md) = find_skill_md(path, 4) {
        if let Some(root) = skill_md.parent() {
            return install_from_skill_root(config_dir, root);
        }
    }
    Err("未找到 SKILL.md（请拖入 skill 文件夹或 zip）".into())
}

pub fn install_skill_zip(config_dir: &Path, zip_path: &Path) -> Result<SkillInfo, String> {
    let tmp = std::env::temp_dir().join(format!(
        "nova-skill-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).map_err(|e| format!("创建临时目录失败：{e}"))?;

    {
        let file = fs::File::open(zip_path).map_err(|e| format!("无法打开 zip：{e}"))?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("zip 损坏：{e}"))?;
        // 安全解压：拒绝绝对路径与 .. 穿越
        for i in 0..archive.len() {
            let mut entry = archive
                .by_index(i)
                .map_err(|e| format!("读取 zip 条目失败：{e}"))?;
            let name = entry
                .enclosed_name()
                .ok_or_else(|| format!("zip 含非法路径：{}", entry.name()))?
                .to_path_buf();
            let out = tmp.join(&name);
            if entry.is_dir() {
                fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            } else {
                if let Some(parent) = out.parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                let mut outfile = fs::File::create(&out).map_err(|e| e.to_string())?;
                io::copy(&mut entry, &mut outfile).map_err(|e| e.to_string())?;
            }
        }
    }

    let result = if is_skill_dir(&tmp) {
        install_from_skill_root(config_dir, &tmp)
    } else if let Some(skill_md) = find_skill_md(&tmp, 5) {
        skill_md
            .parent()
            .ok_or_else(|| "SKILL.md 路径异常".to_string())
            .and_then(|root| install_from_skill_root(config_dir, root))
    } else {
        Err("zip 中未找到 SKILL.md".into())
    };

    let _ = fs::remove_dir_all(&tmp);
    result
}

pub fn remove_skill(config_dir: &Path, name: &str) -> Result<(), String> {
    let name = sanitize_skill_name(name);
    if name.is_empty() || name == "." || name == ".." {
        return Err("无效的 skill 名称".into());
    }
    let dest = skills_dir(config_dir).join(&name);
    if !dest.exists() && !is_any_symlink(&dest) {
        return Err(format!("skill 不存在：{name}"));
    }
    remove_path_quiet(&dest);
    // 清理各后端里指向该 skill 的托管链接
    for root in backend_skill_roots() {
        let link = root.join(&name);
        if is_managed_link(&link, &dest) || (is_any_symlink(&link) && !link.exists()) {
            remove_path_quiet(&link);
        }
    }
    let _ = sync_skills_to_backends(config_dir);
    Ok(())
}

/// 启动后端时调用：从当前构建的数据目录同步 skills。
pub fn sync_skills_from_home() {
    if cfg!(debug_assertions) {
        return;
    }
    if let Some(home) = user_home_dir() {
        let _ = sync_skills_to_backends(&home.join(crate::nova_data_dir_name()));
    }
}

/// 把 `~/.nova/skills/<name>` 以软链接/目录联接同步到各后端全局 skills 目录。
/// 不覆盖用户已有的真实目录；只维护指向 Nova 集中目录的链接。
pub fn sync_skills_to_backends(config_dir: &Path) -> Result<(), String> {
    if cfg!(debug_assertions) {
        return Ok(());
    }
    let central = ensure_skills_dir(config_dir);
    let mut managed_names = Vec::new();
    if let Ok(entries) = fs::read_dir(&central) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !is_skill_dir(&path) {
                continue;
            }
            let name = entry.file_name();
            managed_names.push(name);
        }
    }

    for root in backend_skill_roots() {
        let _ = fs::create_dir_all(&root);
        // 清理失效的托管链接
        if let Ok(entries) = fs::read_dir(&root) {
            for entry in entries.flatten() {
                let link = entry.path();
                if !is_any_symlink(&link) {
                    continue;
                }
                let Some(target) = resolve_link_target(&link) else {
                    continue;
                };
                // 指向 central 下某 skill，但源已不存在 → 删掉
                if target.starts_with(&central) && !is_skill_dir(&target) {
                    remove_path_quiet(&link);
                }
            }
        }

        for name in &managed_names {
            let src = central.join(name);
            let dst = root.join(name);
            if is_managed_link(&dst, &src) {
                continue;
            }
            if dst.exists() || is_any_symlink(&dst) {
                if is_any_symlink(&dst) {
                    // 其它链接：若指向 central 内则重建；否则跳过
                    if let Some(target) = resolve_link_target(&dst) {
                        if target.starts_with(&central) {
                            remove_path_quiet(&dst);
                        } else {
                            continue;
                        }
                    } else {
                        continue;
                    }
                } else {
                    // 真实目录/文件：不覆盖用户自有 skill
                    continue;
                }
            }
            if let Err(e) = link_dir(&src, &dst) {
                eprintln!(
                    "[skills] 链接失败 {} -> {}: {e}",
                    src.display(),
                    dst.display()
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_local_skills_into_isolated_runtime() {
        let root =
            std::env::temp_dir().join(format!("nova-skill-runtime-test-{}", uuid::Uuid::new_v4()));
        let config = root.join("config");
        let skill = skills_dir(&config).join("demo");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "---\nname: demo\n---\n").unwrap();
        std::fs::write(skill.join("helper.txt"), "local").unwrap();

        let destination = root.join("runtime-skills");
        copy_skills_to_runtime(&config, &destination).unwrap();

        assert_eq!(
            std::fs::read_to_string(destination.join("demo").join("helper.txt")).unwrap(),
            "local"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
