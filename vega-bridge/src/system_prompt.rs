//! Vega 系统提示词 —— 移植自 `scripts/alkaid-core.mjs` `buildAlkaidSystemPrompt`。
use crate::skills::{format_alkaid_skills_prompt, Skill};
use std::path::Path;

pub struct ShellConfig {
    pub shell: String,
    pub kind: String,
}

pub fn optimize_system_prompt(stable_parts: &[String], dynamic_parts: &[String]) -> String {
    let stable: String = stable_parts.iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join("\n\n").trim().to_string();
    let dynamic: String = dynamic_parts.iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join("\n\n").trim().to_string();
    if stable.is_empty() { return dynamic; }
    if dynamic.is_empty() { return stable; }
    format!("{stable}\n\n---\n\n{dynamic}")
}

pub fn build_system_prompt(
    cwd: &Path,
    skills: &[Skill],
    read_only: bool,
    shell_config: Option<&ShellConfig>,
    system_prompt: &str,
) -> String {
    let cwd_str = cwd.to_string_lossy().replace('\\', "/");
    let tool_lines: Vec<String> = [
        Some("- read_files: 并行读取多个 UTF-8 文本文件（可带 offset/limit）".to_string()),
        if read_only { None } else { Some("- edit_files: 并行智能编辑多个互不依赖的已有文件（精确优先、锚点定位、歧义拒绝）".to_string()) },
        Some("- read: 读取单个文件".to_string()),
        Some(if read_only {
            "- grep / find / ls: 只读搜索与列举".to_string()
        } else if shell_config.map(|s| s.kind.as_str()) == Some("powershell") {
            "- bash: 执行 PowerShell 命令".to_string()
        } else {
            "- bash: 执行 Bash 命令".to_string()
        }),
        if read_only { None } else { Some("- edit / write: 单文件编辑或写入".to_string()) },
    ]
    .into_iter()
    .flatten()
    .collect();

    let stable_parts = vec![
        "你是 Vega：高效、简单、面向软件工程结果。".to_string(),
        "回复默认简洁专业，使用完整句子并保留必要解释。先给结论，再给行动所需信息。省略寒暄、套话、复述、工具旁白和重复总结。简单问题简答，复杂问题按需展开。不用装饰性表格或长日志，只引关键错误。代码、命令、API 和错误原文须准确；安全警告、不可逆操作确认和必要步骤不得省略。按用户要求增减细节。".to_string(),
        format!("Available tools:\n{}", tool_lines.join("\n")),
        "你拥有批量增强 read_files、edit_files，以及 PI coding agent 的原生 read、bash、edit、write 工具。以下工具选择规则是硬性约束。每次准备读取前，先汇总当前已知目标：仅有一个目标时使用 read；同一读取阶段已有两个及以上路径已知、互不依赖的 UTF-8 文本目标时，必须在一次 read_files 调用中合并读取，并为每个文件分别设置必要的 offset/limit。禁止连续调用多个 read，也禁止用并行封装的多个 read 代替 read_files；想按顺序理解文件不构成读取依赖。只有后一个目标的路径或读取范围必须由前一次结果确定、目标不是 UTF-8 文本，或当前确实仅需一个文件时，才使用 read。后续新发现多个独立文本目标时，下一读取阶段仍须合并使用 read_files。读取内容遵循最小必要原则：已知目标行范围时，只读取相关行段；需要更多上下文时再按需读取相邻行段。未知目标位置时，先用搜索工具定位行号，再读取命中位置附近的必要上下文；大文件禁止无目的全量读取。修改两个及以上互不依赖的已有文件时必须使用 edit_files；同一文件的多处修改合并到该文件的一组 edits。仅在存在先后依赖或目标重叠时串行调用工具。".to_string(),
        "搜索与遍历必须成本有界。禁止使用 `grep -r` 或 `grep -R` 对仓库根目录或源码根目录进行无排除的递归搜索；Git 仓库中搜索已跟踪文件时优先使用 `git grep`，需要搜索未跟踪文件时使用 `rg`，并默认遵守 `.gitignore`。除非任务明确要求，不得扫描构建产物、依赖、缓存、生成文件或大型二进制资源目录。`| head`、`| tail` 和输出截断只限制结果展示，不属于工作量限制；递归命令必须通过限定路径、glob、文件类型或排除目录缩小实际扫描范围，并设置较短的 timeout。递归命令超时后不得原样重试，必须缩小范围或改用更合适的搜索工具。".to_string(),
        "先理解再修改，保持改动聚焦；完成后简洁报告结果和验证。".to_string(),
        "完成修改后，优先根据版本控制 diff 按需确定受影响单元及直接使用方，并执行成本最低且有效的验证；禁止遍历或列出完整仓库、无依据扩大范围，纯文档类改动可说明依据后跳过测试，无法验证时须报告原因、建议命令及剩余风险。".to_string(),
        if let Some(sc) = shell_config {
            if sc.kind == "powershell" {
                format!("命令终端已确认使用 PowerShell（{}）；bash 工具在 Windows 下通过 PowerShell 执行命令，必须从第一次调用起使用 PowerShell 语法（cmdlet、`;` 串联多条命令、`$env:NAME` 访问环境变量），不要使用 Bash 语法（`export`、`&&` 串联在 Windows PowerShell 5.1 中不可用、POSIX 风格的 sed/awk/grep 调用）。", sc.shell)
            } else {
                format!("命令终端已确认使用 Bash（{}）；bash 工具必须从第一次调用起使用 Bash 语法，不要使用 PowerShell cmdlet。", sc.shell)
            }
        } else {
            String::new()
        },
    ];

    let dynamic_parts = vec![
        if read_only { "当前为计划模式：只读分析，不得修改文件。".to_string() } else { String::new() },
        format!("Current working directory: {cwd_str}"),
        format_alkaid_skills_prompt(skills),
        system_prompt.to_string(),
    ];

    optimize_system_prompt(&stable_parts, &dynamic_parts)
}

/// Windows PowerShell detection mirroring `findWindowsPowerShell`.
pub fn find_windows_powershell() -> Option<String> {
    let roots: Vec<String> = std::env::var("SystemRoot").ok().into_iter().chain(std::env::var("windir").ok()).collect();
    for root in roots {
        let candidate = Path::new(&root).join("System32").join("WindowsPowerShell").join("v1.0").join("powershell.exe");
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into());
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("powershell.exe");
            if candidate.is_file() {
                return Some(candidate.to_string_lossy().into());
            }
        }
    }
    None
}

pub fn detect_shell_config() -> ShellConfig {
    #[cfg(windows)]
    {
        if let Some(shell) = find_windows_powershell() {
            return ShellConfig { shell, kind: "powershell".into() };
        }
    }
    // Fallback: bash with `-c`.
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "bash".into());
    ShellConfig { shell, kind: "bash".into() }
}

pub fn resolve_shell_config(shell_config: ShellConfig) -> ShellConfig {
    #[cfg(windows)]
    {
        let shim = if shell_config.kind == "powershell" {
            std::env::var("NOVA_SHELL_SHIM_POWERSHELL").ok()
        } else {
            std::env::var("NOVA_SHELL_SHIM_BASH").ok()
        };
        if let Some(shim) = shim.filter(|s| !s.is_empty()) {
            return ShellConfig { shell: shim, kind: shell_config.kind };
        }
    }
    let _ = shell_config.kind.clone();
    shell_config
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Mutex;

    /// Serializes env-var access across shell-config tests (env vars are process-global
    /// and tests run in parallel by default).
    static SHELL_ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn optimize_system_prompt_joins_stable_and_dynamic() {
        let stable = vec!["stable part".to_string()];
        let dynamic = vec!["dynamic part".to_string()];
        let prompt = optimize_system_prompt(&stable, &dynamic);
        assert!(prompt.contains("stable part"));
        assert!(prompt.contains("dynamic part"));
        assert!(prompt.contains("---"));
    }

    #[test]
    fn optimize_system_prompt_empty_stable_returns_dynamic() {
        let stable = vec![String::new()];
        let dynamic = vec!["only dynamic".to_string()];
        let prompt = optimize_system_prompt(&stable, &dynamic);
        assert_eq!(prompt, "only dynamic");
    }

    #[test]
    fn optimize_system_prompt_empty_dynamic_returns_stable() {
        let stable = vec!["only stable".to_string()];
        let dynamic = vec![String::new()];
        let prompt = optimize_system_prompt(&stable, &dynamic);
        assert_eq!(prompt, "only stable");
    }

    #[test]
    fn build_system_prompt_includes_cwd_and_tools() {
        let cwd = PathBuf::from("/tmp/project");
        let prompt = build_system_prompt(&cwd, &[], false, None, "");
        assert!(prompt.contains("Current working directory: /tmp/project"));
        assert!(prompt.contains("read_files"));
        assert!(prompt.contains("edit_files"));
        assert!(prompt.contains("bash"));
    }

    #[test]
    fn build_system_prompt_read_only_hides_edit_tools() {
        let cwd = PathBuf::from("/tmp/project");
        let prompt = build_system_prompt(&cwd, &[], true, None, "");
        assert!(prompt.contains("read_files"));
        // The "Available tools" list must not include the edit_files tool line.
        assert!(!prompt.contains("- edit_files:"));
        assert!(!prompt.contains("- edit / write:"));
        assert!(prompt.contains("计划模式"));
        assert!(prompt.contains("只读"));
    }

    #[test]
    fn build_system_prompt_powershell_branch_mentions_powershell() {
        let cwd = PathBuf::from("C:/Users/test/proj");
        let shell = ShellConfig { shell: "powershell.exe".into(), kind: "powershell".into() };
        let prompt = build_system_prompt(&cwd, &[], false, Some(&shell), "");
        assert!(prompt.contains("PowerShell"));
        assert!(prompt.contains("powershell.exe"));
        // CWD backslashes are normalized to forward slashes.
        assert!(prompt.contains("C:/Users/test/proj"));
    }

    #[test]
    fn build_system_prompt_bash_branch_mentions_bash() {
        let cwd = PathBuf::from("/home/user/proj");
        let shell = ShellConfig { shell: "/bin/bash".into(), kind: "bash".into() };
        let prompt = build_system_prompt(&cwd, &[], false, Some(&shell), "");
        assert!(prompt.contains("Bash"));
        assert!(prompt.contains("/bin/bash"));
    }

    #[test]
    fn build_system_prompt_appends_custom_instructions() {
        let cwd = PathBuf::from("/tmp/proj");
        let prompt = build_system_prompt(&cwd, &[], false, None, "Always reply in Chinese.");
        assert!(prompt.contains("Always reply in Chinese."));
    }

    #[test]
    fn resolve_shell_config_no_shim_returns_original() {
        // Clear any shim env vars so the fallback path is exercised.
        // NOTE: env vars are process-global and tests run in parallel. We use a
        // critical-section mutex (defined below) to serialize env-var access across
        // all shell-config tests.
        let _guard = SHELL_ENV_LOCK.lock().unwrap();
        std::env::remove_var("NOVA_SHELL_SHIM_POWERSHELL");
        std::env::remove_var("NOVA_SHELL_SHIM_BASH");
        let original = ShellConfig { shell: "powershell.exe".into(), kind: "powershell".into() };
        let resolved = resolve_shell_config(ShellConfig { shell: "powershell.exe".into(), kind: "powershell".into() });
        assert_eq!(resolved.shell, original.shell);
        assert_eq!(resolved.kind, original.kind);
    }

    /// All env-var-dependent shim resolution cases in one test to avoid parallel races
    /// (env vars are process-global).
    #[cfg(windows)]
    #[test]
    fn resolve_shell_config_shim_env_cases() {
        let _guard = SHELL_ENV_LOCK.lock().unwrap();
        // Powershell shim overrides.
        std::env::set_var("NOVA_SHELL_SHIM_POWERSHELL", "C:/shim/nova-shell-shim.exe");
        let resolved_ps = resolve_shell_config(ShellConfig { shell: "powershell.exe".into(), kind: "powershell".into() });
        assert_eq!(resolved_ps.shell, "C:/shim/nova-shell-shim.exe");
        assert_eq!(resolved_ps.kind, "powershell");

        // Bash shim overrides.
        std::env::set_var("NOVA_SHELL_SHIM_BASH", "C:/shim/bash-shim.exe");
        let resolved_bash = resolve_shell_config(ShellConfig { shell: "bash".into(), kind: "bash".into() });
        assert_eq!(resolved_bash.shell, "C:/shim/bash-shim.exe");
        assert_eq!(resolved_bash.kind, "bash");

        // Empty shim falls back to original.
        std::env::set_var("NOVA_SHELL_SHIM_POWERSHELL", "");
        let resolved_empty = resolve_shell_config(ShellConfig { shell: "powershell.exe".into(), kind: "powershell".into() });
        assert_eq!(resolved_empty.shell, "powershell.exe");

        // Cleanup.
        std::env::remove_var("NOVA_SHELL_SHIM_POWERSHELL");
        std::env::remove_var("NOVA_SHELL_SHIM_BASH");
    }

    #[cfg(windows)]
    #[test]
    fn find_windows_powershell_locates_system_shell() {
        // On a real Windows host, SystemRoot points to C:\Windows, so this should find
        // the canonical powershell.exe. On non-Windows hosts this test is absent.
        let found = find_windows_powershell();
        assert!(found.is_some(), "expected powershell.exe to be discovered on Windows");
        let path = found.unwrap();
        assert!(path.to_lowercase().ends_with("powershell.exe"));
    }

    #[cfg(windows)]
    #[test]
    fn detect_shell_config_returns_powershell_on_windows() {
        // Ensure no shim env var interferes.
        let _guard = SHELL_ENV_LOCK.lock().unwrap();
        std::env::remove_var("NOVA_SHELL_SHIM_POWERSHELL");
        let config = detect_shell_config();
        assert_eq!(config.kind, "powershell");
        assert!(config.shell.to_lowercase().ends_with("powershell.exe"));
    }
}
