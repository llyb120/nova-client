//! Git worktree 操作：为会话创建/移除独立工作目录 + 分支，隔离执行不干扰主工作区。
//! 全部通过命令行 git 实现（依赖用户已安装 git 且在 PATH 中）。

use std::path::Path;
use std::process::Command;

/// Windows 下不弹出控制台窗口
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 在 repo 目录执行一条 git 子命令，成功返回 stdout（已 trim），失败返回 stderr。
pub fn run(repo: &str, args: &[&str]) -> Result<String, String> {
    Ok(git_stdout(repo, args)?.trim().to_string())
}

/// 在 repo 目录执行一条 git 子命令，成功返回原始 stdout（不 trim），失败返回 stderr。
/// 用于 `git show` / `git diff` 等需要保留文件尾部换行的场景。
pub fn run_raw(repo: &str, args: &[&str]) -> Result<String, String> {
    git_stdout(repo, args)
}

/// 在 repo 目录执行一条 git 子命令，成功返回 stdout，失败返回 stderr。
fn git_stdout(repo: &str, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo).args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("执行 git 失败：{e}（请确认已安装 git 并在 PATH 中）"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("git {}：{}", args.join(" "), err.trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 目录是否位于某个 git 工作树内
pub fn is_repo(dir: &str) -> bool {
    if !Path::new(dir).is_dir() {
        return false;
    }
    run(dir, &["rev-parse", "--is-inside-work-tree"])
        .map(|s| s == "true")
        .unwrap_or(false)
}

/// 返回 dir 所属仓库的根目录（顶层工作树）
pub fn repo_root(dir: &str) -> Result<String, String> {
    let root = run(dir, &["rev-parse", "--show-toplevel"])?;
    if root.is_empty() {
        return Err("不是 git 仓库".into());
    }
    // git 统一返回正斜杠，Windows 下规范化为反斜杠，避免与本地路径比较不一致
    #[cfg(windows)]
    {
        Ok(root.replace('/', "\\"))
    }
    #[cfg(not(windows))]
    {
        Ok(root)
    }
}

/// 校验分支名是否合法（交给 git check-ref-format 判定）
pub fn valid_branch(branch: &str) -> bool {
    let branch = branch.trim();
    if branch.is_empty() {
        return false;
    }
    let mut cmd = Command::new("git");
    cmd.args(["check-ref-format", "--branch", branch]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.output().map(|o| o.status.success()).unwrap_or(false)
}

/// 检查新分支名是否与已有分支冲突，冲突时返回中文错误消息（否则 None）：
/// - 完全同名；
/// - D/F（目录/文件）冲突：git 中同一名字不能既是分支又是目录，
///   即已有分支是新分支的父段（已有 `test`，新建 `test/abc`），或反之（已有 `test/abc`，新建 `test`）。
pub fn branch_conflict(repo: &str, branch: &str) -> Option<String> {
    let branch = branch.trim();
    let existing = list_branches(repo).unwrap_or_default();
    for e in &existing {
        if e == branch {
            return Some(format!("分支已存在：{branch}，请换一个名字"));
        }
        if branch.starts_with(&format!("{e}/")) || e.starts_with(&format!("{branch}/")) {
            return Some(format!(
                "分支名与已有分支「{e}」冲突：git 不允许同名的分支和目录并存（{e} 与 {branch}）。请改用其它新分支名。"
            ));
        }
    }
    None
}

/// 列出仓库的本地分支（refs/heads），按名称排序。
pub fn list_branches(dir: &str) -> Result<Vec<String>, String> {
    let out = run(
        dir,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )?;
    let mut v: Vec<String> = out
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    v.sort();
    Ok(v)
}

/// 当前分支名；分离 HEAD 时返回空串。
pub fn current_branch(dir: &str) -> String {
    run(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]).unwrap_or_default()
}

/// 创建 worktree：
/// - branch 非空：在 repo 上基于 base（commit-ish，空则用 HEAD）新建分支 branch，检出到 path；
/// - branch 空：不新建分支，直接把 base（已有分支/commit-ish）检出到 path。
pub fn add(repo: &str, path: &str, branch: &str, base: &str) -> Result<(), String> {
    let branch = branch.trim();
    let base = base.trim();
    let base = if base.is_empty() { "HEAD" } else { base };
    if branch.is_empty() {
        run(repo, &["worktree", "add", path, base])?;
    } else {
        run(repo, &["worktree", "add", "-b", branch, path, base])?;
    }
    Ok(())
}

/// 分支是否已被某个工作树检出（含主工作区）；是则返回那个工作树的路径。
/// git 不允许同一分支同时检出到两个工作树，直接使用已有分支前需要预检。
pub fn branch_checked_out(repo: &str, branch: &str) -> Option<String> {
    let out = run(repo, &["worktree", "list", "--porcelain"]).ok()?;
    let mut cur_path: Option<String> = None;
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            cur_path = Some(p.trim().to_string());
        } else if let Some(b) = line.strip_prefix("branch ") {
            let short = b.trim().strip_prefix("refs/heads/").unwrap_or(b.trim());
            if short == branch {
                return cur_path;
            }
        }
    }
    None
}

/// 工作树是否干净（无未提交改动/未跟踪文件）。
pub fn is_clean(dir: &str) -> Result<bool, String> {
    Ok(run(dir, &["status", "--porcelain"])?.is_empty())
}

/// 检出已有分支（工作树需干净，调用方自行预检）。
pub fn checkout(dir: &str, branch: &str) -> Result<(), String> {
    run(dir, &["checkout", branch])?;
    Ok(())
}

/// 基于 base（空 = HEAD）新建并检出分支（工作树需干净，调用方自行预检）。
pub fn checkout_new_branch(dir: &str, branch: &str, base: &str) -> Result<(), String> {
    let base = base.trim();
    if base.is_empty() {
        run(dir, &["checkout", "-b", branch])?;
    } else {
        run(dir, &["checkout", "-b", branch, base])?;
    }
    Ok(())
}

/// 是否有任何未提交改动或未跟踪文件。
pub fn has_changes(dir: &str) -> Result<bool, String> {
    Ok(!run(dir, &["status", "--porcelain"])?.is_empty())
}

/// 提交当前工作树全部变更。无变更时直接返回 Ok(false)。
pub fn commit_all(dir: &str, message: &str) -> Result<bool, String> {
    if !has_changes(dir)? {
        return Ok(false);
    }
    run(dir, &["add", "-A"])?;
    run(dir, &["commit", "-m", message])?;
    Ok(true)
}

/// 在 dir 执行 `git merge <branch>`。成功返回 Ok；失败返回 stderr（冲突与否由调用方查 has_conflicts）。
pub fn merge(dir: &str, branch: &str) -> Result<(), String> {
    run(dir, &["merge", "--no-edit", branch])?;
    Ok(())
}

/// 工作树当前是否存在未解决的合并冲突（index 里有 unmerged 条目）。
pub fn has_conflicts(dir: &str) -> bool {
    run(dir, &["ls-files", "-u"])
        .map(|s| !s.is_empty())
        .unwrap_or(false)
}

/// 中止一次失败的合并（best-effort，工作树没有进行中的合并时静默忽略）。
pub fn merge_abort(dir: &str) {
    let _ = run(dir, &["merge", "--abort"]);
}

/// 移除 worktree 工作目录（--force：容忍未提交改动/锁定）。
pub fn remove(repo: &str, path: &str) -> Result<(), String> {
    run(repo, &["worktree", "remove", "--force", path])?;
    Ok(())
}

/// 删除分支（-D 强制，忽略未合并检查）。
pub fn delete_branch(repo: &str, branch: &str) -> Result<(), String> {
    run(repo, &["branch", "-D", branch])?;
    Ok(())
}
