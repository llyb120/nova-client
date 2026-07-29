//! Vega/Alkaid system prompt assembly, ported from `buildAlkaidSystemPrompt`
//! and `optimizeAlkaidSystemPrompt` in `alkaid-core.mjs`.
//!
//! Skill formatting (`formatAlkaidSkillsPrompt`) lives in milestone M2 with the
//! rest of the pi-coding-agent tooling; here the caller supplies the already
//! formatted `skills_prompt` (empty string when there are no skills).

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShellKind {
    Bash,
    Powershell,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ShellConfig {
    pub shell: String,
    pub kind: ShellKind,
}

/// Port of `optimizeAlkaidSystemPrompt(stableParts, dynamicParts)`: drop empty
/// parts, join each group with blank lines, trim, and separate the two groups
/// with a `---` rule when both are present.
fn optimize(stable_parts: &[String], dynamic_parts: &[String]) -> String {
    let stable = stable_parts
        .iter()
        .filter(|part| !part.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n\n");
    let stable = stable.trim();
    let dynamic = dynamic_parts
        .iter()
        .filter(|part| !part.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n\n");
    let dynamic = dynamic.trim();
    if stable.is_empty() {
        return dynamic.to_string();
    }
    if dynamic.is_empty() {
        return stable.to_string();
    }
    format!("{stable}\n\n---\n\n{dynamic}")
}

/// Port of `buildAlkaidSystemPrompt(options)`. `cwd` is normalized to forward
/// slashes; `skills_prompt` is the preformatted skills block ("" for none).
pub fn build_system_prompt(
    cwd: &str,
    read_only: bool,
    shell_config: Option<&ShellConfig>,
    skills_prompt: &str,
    system_prompt: &str,
) -> String {
    let cwd = cwd.replace('\\', "/");

    let mut tool_lines: Vec<&str> = Vec::new();
    tool_lines.push("- read_files: 并行读取多个 UTF-8 文本文件（可带 offset/limit）");
    if !read_only {
        tool_lines.push("- edit_files: 并行智能编辑多个互不依赖的已有文件（精确优先、锚点定位、歧义拒绝）");
    }
    tool_lines.push("- read: 读取单个文件");
    let bash_line = if read_only {
        "- grep / find / ls: 只读搜索与列举"
    } else if matches!(shell_config, Some(config) if config.kind == ShellKind::Powershell) {
        "- bash: 执行 PowerShell 命令"
    } else {
        "- bash: 执行 Bash 命令"
    };
    tool_lines.push(bash_line);
    if !read_only {
        tool_lines.push("- edit / write: 单文件编辑或写入");
    }
    let available_tools = format!("Available tools:\n{}", tool_lines.join("\n"));

    let shell_part = match shell_config {
        Some(config) if config.kind == ShellKind::Powershell => format!(
            "命令终端已确认使用 PowerShell（{}）；bash 工具在 Windows 下通过 PowerShell 执行命令，必须从第一次调用起使用 PowerShell 语法（cmdlet、`;` 串联多条命令、`$env:NAME` 访问环境变量），不要使用 Bash 语法（`export`、`&&` 串联在 Windows PowerShell 5.1 中不可用、POSIX 风格的 sed/awk/grep 调用）。",
            config.shell
        ),
        Some(config) => format!(
            "命令终端已确认使用 Bash（{}）；bash 工具必须从第一次调用起使用 Bash 语法，不要使用 PowerShell cmdlet。",
            config.shell
        ),
        None => String::new(),
    };

    let stable_parts: Vec<String> = vec![
        "你是 Vega：高效、简单、面向软件工程结果。".to_string(),
        "回复默认简洁专业，使用完整句子并保留必要解释。先给结论，再给行动所需信息。省略寒暄、套话、复述、工具旁白和重复总结。简单问题简答，复杂问题按需展开。不用装饰性表格或长日志，只引关键错误。代码、命令、API 和错误原文须准确；安全警告、不可逆操作确认和必要步骤不得省略。按用户要求增减细节。".to_string(),
        available_tools,
        "你拥有批量增强 read_files、edit_files，以及 PI coding agent 的原生 read、bash、edit、write 工具。以下工具选择规则是硬性约束。每次准备读取前，先汇总当前已知目标：仅有一个目标时使用 read；同一读取阶段已有两个及以上路径已知、互不依赖的 UTF-8 文本目标时，必须在一次 read_files 调用中合并读取，并为每个文件分别设置必要的 offset/limit。禁止连续调用多个 read，也禁止用并行封装的多个 read 代替 read_files；想按顺序理解文件不构成读取依赖。只有后一个目标的路径或读取范围必须由前一次结果确定、目标不是 UTF-8 文本，或当前确实仅需一个文件时，才使用 read。后续新发现多个独立文本目标时，下一读取阶段仍须合并使用 read_files。读取内容遵循最小必要原则：已知目标行范围时，只读取相关行段；需要更多上下文时再按需读取相邻行段。未知目标位置时，先用搜索工具定位行号，再读取命中位置附近的必要上下文；大文件禁止无目的全量读取。修改两个及以上互不依赖的已有文件时必须使用 edit_files；同一文件的多处修改合并到该文件的一组 edits。仅在存在先后依赖或目标重叠时串行调用工具。".to_string(),
        "搜索与遍历必须成本有界。禁止使用 `grep -r` 或 `grep -R` 对仓库根目录或源码根目录进行无排除的递归搜索；Git 仓库中搜索已跟踪文件时优先使用 `git grep`，需要搜索未跟踪文件时使用 `rg`，并默认遵守 `.gitignore`。除非任务明确要求，不得扫描构建产物、依赖、缓存、生成文件或大型二进制资源目录。`| head`、`| tail` 和输出截断只限制结果展示，不属于工作量限制；递归命令必须通过限定路径、glob、文件类型或排除目录缩小实际扫描范围，并设置较短的 timeout。递归命令超时后不得原样重试，必须缩小范围或改用更合适的搜索工具。".to_string(),
        "先理解再修改，保持改动聚焦；完成后简洁报告结果和验证。".to_string(),
        "完成修改后，优先根据版本控制 diff 按需确定受影响单元及直接使用方，并执行成本最低且有效的验证；禁止遍历或列出完整仓库、无依据扩大范围，纯文档类改动可说明依据后跳过测试，无法验证时须报告原因、建议命令及剩余风险。".to_string(),
        shell_part,
    ];

    let dynamic_parts: Vec<String> = vec![
        if read_only {
            "当前为计划模式：只读分析，不得修改文件。".to_string()
        } else {
            String::new()
        },
        format!("Current working directory: {cwd}"),
        skills_prompt.to_string(),
        system_prompt.to_string(),
    ];

    optimize(&stable_parts, &dynamic_parts)
}
