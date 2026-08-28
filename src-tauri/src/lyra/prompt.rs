//! 系统提示（稳定/动态分段，配合 prompt_cache_key 命中前缀缓存）、技能加载、
//! 工具结果治理（超限归档 + 首尾截断）、provider 重试判定与用量合并。

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Reasonix 风格的单工具上下文预算；超限文本先归档再截断。
pub const TOOL_OUTPUT_CONTEXT_MAX_BYTES: usize = 32 * 1024;
/// polaris 有自己的完整单元预算（默认 32KB，显式上限 64KB）。
pub const POLARIS_OUTPUT_MAX_BYTES: usize = 64 * 1024;
/// OpenAI Responses function_call_output.output 硬限制，预留截断提示空间。
pub const OPENAI_TOOL_OUTPUT_SAFE_MAX_CHARS: usize = 10_485_760 - 512;
const PROMPT_CACHE_KEY_MAX_CHARS: usize = 64;
const SKILL_COMPRESSION_MIN_COUNT: usize = 4;

pub fn image_media_type(path: &str) -> Option<&'static str> {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_lowercase();
    match ext.as_str() {
        ".gif" => Some("image/gif"),
        ".jpeg" | ".jpg" => Some("image/jpeg"),
        ".png" => Some("image/png"),
        ".webp" => Some("image/webp"),
        _ => None,
    }
}

fn safe_segment(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '-'
            }
        })
        .take(96)
        .collect();
    if cleaned.is_empty() {
        "tool".into()
    } else {
        cleaned
    }
}

fn truncate_utf8_head(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn truncate_utf8_tail(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut start = text.len() - max_bytes;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// 归档超限工具输出，返回稳定的首/尾占位文本（60% 头 + 40% 尾）。
pub fn govern_tool_text(
    text: &str,
    max_bytes: usize,
    archive_dir: Option<&Path>,
    tool_call_id: &str,
    tool_name: &str,
) -> (String, Option<String>, usize) {
    let original_bytes = text.len();
    if original_bytes <= max_bytes {
        return (text.to_string(), None, original_bytes);
    }
    let mut archive_path = None;
    if let Some(dir) = archive_dir {
        if std::fs::create_dir_all(dir).is_ok() {
            let path = dir.join(format!(
                "{}-{}.txt",
                safe_segment(tool_call_id),
                safe_segment(tool_name)
            ));
            if std::fs::write(&path, text).is_ok() {
                archive_path = Some(path.display().to_string());
            }
        }
    }
    let location = archive_path
        .as_ref()
        .map(|path| format!(" archived at {path}; use read with offset/limit to inspect it"))
        .unwrap_or_default();
    let notice = format!("\n\n…[elided tool result — {original_bytes} bytes{location}]\n\n");
    let budget = max_bytes.saturating_sub(notice.len());
    let head_budget = budget * 3 / 5 + budget % 3 / 5;
    let tail_budget = budget.saturating_sub(head_budget);
    let governed = format!(
        "{}{}{}",
        truncate_utf8_head(text, head_budget),
        notice,
        truncate_utf8_tail(text, tail_budget)
    );
    (governed, archive_path, original_bytes)
}

/// 兜底：OpenAI 对单条工具输出有 10MB 字符上限。
pub fn clamp_tool_output_text(text: &str) -> String {
    if text.chars().count() <= OPENAI_TOOL_OUTPUT_SAFE_MAX_CHARS {
        return text.to_string();
    }
    let notice = format!(
        "\n\n…[truncated: tool output exceeded {OPENAI_TOOL_OUTPUT_SAFE_MAX_CHARS} chars; original length {}]",
        text.chars().count()
    );
    let keep = OPENAI_TOOL_OUTPUT_SAFE_MAX_CHARS.saturating_sub(notice.chars().count());
    let head: String = text.chars().take(keep).collect();
    format!("{head}{notice}")
}

pub fn clamp_prompt_cache_key(key: &str) -> Option<String> {
    let normalized = key.trim();
    if normalized.is_empty() {
        return None;
    }
    let chars: Vec<char> = normalized.chars().collect();
    if chars.len() <= PROMPT_CACHE_KEY_MAX_CHARS {
        return Some(normalized.to_string());
    }
    Some(chars[..PROMPT_CACHE_KEY_MAX_CHARS].iter().collect())
}

const RETRYABLE_FRAGMENTS: &[&str] = &[
    "terminated",
    "fetch failed",
    "connection error",
    "socket hang up",
    "econnreset",
    "etimedout",
    "econnaborted",
    "epipe",
    "request timed out",
    "premature close",
    "other side closed",
    "network connection lost",
    "upstream stream ended prematurely",
    "safe to retry",
    "stream ended before a terminal response event",
    "stream ended without finish_reason",
    "error decoding response body",
    "idle timeout",
    "sse 连续",
    "429",
    "too many requests",
    "rate limit",
    "503",
    "service unavailable",
    "timed out",
    "error sending request",
];

pub fn is_retryable_provider_error(error: &str) -> bool {
    let message = error.to_lowercase();
    RETRYABLE_FRAGMENTS
        .iter()
        .any(|fragment| message.contains(fragment))
}

/// Provider 对上下文超限的文案并不统一；单独识别后由 Reasonix 强制收缩轨迹并重试一次。
pub fn is_context_window_error(error: &str) -> bool {
    let message = error.to_lowercase();
    [
        "exceeds the context window",
        "context window exceeded",
        "context length exceeded",
        "maximum context length",
        "context_length_exceeded",
        "prompt is too long",
        "input is too long",
        "too many tokens",
        "request too large for model",
    ]
    .iter()
    .any(|fragment| message.contains(fragment))
}

pub const PROVIDER_RETRY_DELAYS_MS: &[u64] = &[1_000, 3_000];

/// 按 PI 约定累加一次 agent 轮内多次模型请求的用量。
pub fn merge_usage(total: &mut Value, usage: &Value) {
    for key in ["input", "output", "cacheRead", "cacheWrite"] {
        let add = usage.get(key).and_then(Value::as_u64).unwrap_or(0);
        let current = total.get(key).and_then(Value::as_u64).unwrap_or(0);
        total[key] = json!(current + add);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ShellKind {
    Bash,
    PowerShell,
}

#[derive(Debug, Clone)]
pub struct ShellConfig {
    pub program: String,
    pub kind: ShellKind,
}

pub fn detect_shell() -> ShellConfig {
    if cfg!(windows) {
        // 优先 PowerShell 7 (pwsh.exe)：默认 UTF-8；未安装时回退 Windows PowerShell 5.1。
        #[cfg(windows)]
        if let Some(pwsh) = find_pwsh() {
            return ShellConfig {
                program: pwsh,
                kind: ShellKind::PowerShell,
            };
        }
        for root in [
            std::env::var("SystemRoot").ok(),
            std::env::var("windir").ok(),
        ]
        .into_iter()
        .flatten()
        {
            let candidate = PathBuf::from(root)
                .join("System32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe");
            if candidate.exists() {
                return ShellConfig {
                    program: candidate.display().to_string(),
                    kind: ShellKind::PowerShell,
                };
            }
        }
        return ShellConfig {
            program: "powershell.exe".into(),
            kind: ShellKind::PowerShell,
        };
    }
    for candidate in ["/bin/bash", "/usr/bin/bash", "/bin/sh"] {
        if Path::new(candidate).exists() {
            return ShellConfig {
                program: candidate.into(),
                kind: ShellKind::Bash,
            };
        }
    }
    ShellConfig {
        program: "sh".into(),
        kind: ShellKind::Bash,
    }
}

#[cfg(windows)]
fn find_pwsh() -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("pwsh.exe");
        if candidate.is_file() {
            return Some(candidate.display().to_string());
        }
    }
    for key in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
        if let Ok(root) = std::env::var(key) {
            let candidate = PathBuf::from(root)
                .join("PowerShell")
                .join("7")
                .join("pwsh.exe");
            if candidate.is_file() {
                return Some(candidate.display().to_string());
            }
        }
    }
    None
}

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

/// 从 SKILL.md frontmatter 解析 name/description（Agent Skills 标准）。
fn parse_skill(path: &Path) -> Option<Skill> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut name = None;
    let mut description = None;
    let trimmed = text.trim_start();
    if let Some(rest) = trimmed.strip_prefix("---") {
        let end = rest.find("\n---")?;
        for line in rest[..end].lines() {
            let line = line.trim();
            if let Some(value) = line.strip_prefix("name:") {
                name = Some(
                    value
                        .trim()
                        .trim_matches('"')
                        .trim_matches('\'')
                        .to_string(),
                );
            } else if let Some(value) = line.strip_prefix("description:") {
                description = Some(
                    value
                        .trim()
                        .trim_matches('"')
                        .trim_matches('\'')
                        .to_string(),
                );
            }
        }
    }
    let name = name.or_else(|| {
        path.parent()
            .and_then(|dir| dir.file_name())
            .and_then(|name| name.to_str())
            .map(str::to_string)
    })?;
    Some(Skill {
        name,
        description: description.unwrap_or_default(),
        path: path.to_path_buf(),
    })
}

/// 技能目录（<root>/alkaid/skills/<name>/SKILL.md）。
pub fn load_skills(roots: &crate::lyra::config::Roots) -> Vec<Skill> {
    let root = roots.skills();
    let mut skills = Vec::new();
    let Ok(entries) = std::fs::read_dir(&root) else {
        return skills;
    };
    for entry in entries.flatten() {
        let candidate = entry.path().join("SKILL.md");
        if candidate.exists() {
            if let Some(skill) = parse_skill(&candidate) {
                skills.push(skill);
            }
        }
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

pub fn format_skills_prompt(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut lines = vec![
        "The following skills are available for use with the Skill tool:".to_string(),
        String::new(),
    ];
    if skills.len() >= SKILL_COMPRESSION_MIN_COUNT {
        // 数量较多时压缩为单行索引，模型按需自行 read 对应 SKILL.md。
        for skill in skills {
            lines.push(format!(
                "- {}: {} ({})",
                skill.name,
                skill.description,
                skill.path.display()
            ));
        }
    } else {
        for skill in skills {
            lines.push(format!("## {}", skill.name));
            if !skill.description.is_empty() {
                lines.push(skill.description.clone());
            }
            lines.push(format!("Skill file: {}", skill.path.display()));
            lines.push(String::new());
        }
    }
    lines.join("\n")
}

/// 展开 /skill:<name> 调用：替换为技能文件路径提示。
pub fn is_browser_command(text: &str) -> bool {
    text.trim_start()
        .strip_prefix("/browser")
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
}

pub fn is_browser_exit_command(text: &str) -> bool {
    text.trim().eq_ignore_ascii_case("/browser-exit")
}

pub fn expand_browser_command(text: &str) -> Option<String> {
    let rest = text.trim_start().strip_prefix("/browser")?;
    if rest
        .chars()
        .next()
        .is_some_and(|character| !character.is_whitespace())
    {
        return None;
    }
    let task = rest.trim();
    let mut expanded = "用户已进入持续的浏览器调试模式。首个动作优先直接调用 browser open 打开用户给出的网址，不要在页面打开前扫描代码仓库；页面打开后先简短反馈已就绪。只有用户同时给了明确调试目标时，才继续结合代码工具与 browser 工具形成修改代码 → 观察页面/错误 → 截图验收的调试闭环。浏览器标签页会跨后续轮次保留，不要在每轮结束时关闭；只有用户发送 /browser-exit 或明确要求关闭时才 close。".to_string();
    if task.is_empty() {
        expanded.push_str("如果用户尚未提供网址或调试目标，先询问用户。");
    } else {
        expanded.push_str(&format!("\n\n任务：{task}"));
    }
    Some(expanded)
}

pub fn expand_browser_exit_command(text: &str) -> Option<String> {
    is_browser_exit_command(text).then(|| {
        "用户已退出浏览器调试模式。请关闭当前 browser 标签页，并确认后续轮次不再使用 Playwright 工具。"
            .to_string()
    })
}

pub fn expand_skill_command(text: &str, skills: &[Skill]) -> String {
    let Some(rest) = text.trim_start().strip_prefix("/skill:") else {
        return text.to_string();
    };
    let mut parts = rest.trim_start().splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or_default();
    let args = parts.next().unwrap_or_default().trim();
    let Some(skill) = skills.iter().find(|skill| skill.name == name) else {
        return text.to_string();
    };
    let mut expanded = format!(
        "请使用 {} 技能完成下面的任务。先用 read 阅读技能文件 {} 并严格遵循其中的说明。",
        skill.name,
        skill.path.display()
    );
    if !args.is_empty() {
        expanded.push_str(&format!("\n\n任务：{args}"));
    }
    expanded
}

/// 应用级 AGENTS.md（共享数据目录）。
pub fn load_agent_instructions(roots: &crate::lyra::config::Roots) -> String {
    std::fs::read_to_string(roots.data().join("AGENTS.md"))
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// ponytail 极简模式：开启后在 stable 段注入「懒惰资深工程师」规则。
/// 用 DietrichGebert/ponytail 的 AGENTS.md 完整原文（英文）。与"先理解再修改"不冲突。
pub const PONYTAIL_RULES: &str = r#"You are a lazy senior developer. Lazy means efficient, not careless. The best code is the code never written.

Before writing any code, stop at the first rung that holds:

1. Does this need to be built at all? (YAGNI)
2. Does it already exist in this codebase? Reuse the helper, util, or pattern that's already here, don't re-write it.
3. Does the standard library already do this? Use it.
4. Does a native platform feature cover it? Use it.
5. Does an already-installed dependency solve it? Use it.
6. Can this be one line? Make it one line.
7. Only then: write the minimum code that works.

The ladder runs after you understand the problem, not instead of it: read the task and the code it touches, trace the real flow end to end, then climb.

Bug fix = root cause, not symptom: a report names a symptom. Grep every caller of the function you touch and fix the shared function once — one guard there is a smaller diff than one per caller, and patching only the path the ticket names leaves a sibling caller still broken.

Rules:

- No abstractions that weren't explicitly requested.
- No new dependency if it can be avoided.
- No boilerplate nobody asked for.
- Deletion over addition. Boring over clever. Fewest files possible.
- Shortest working diff wins, but only once you understand the problem. The smallest change in the wrong place isn't lazy, it's a second bug.
- Question complex requests: "Do you actually need X, or does Y cover it?"
- Pick the edge-case-correct option when two stdlib approaches are the same size, lazy means less code, not the flimsier algorithm.
- Mark deliberate simplifications that cut a real corner with a known ceiling (global lock, O(n²) scan, naive heuristic) with a `ponytail:` comment naming the ceiling and upgrade path.

Not lazy about: understanding the problem (read it fully and trace the real flow before picking a rung, a small diff you don't understand is just laziness dressed up as efficiency), input validation at trust boundaries, error handling that prevents data loss, security, accessibility, the calibration real hardware needs (the platform is never the spec ideal, a clock drifts, a sensor reads off), anything explicitly requested. Lazy code without its check is unfinished: non-trivial logic leaves ONE runnable check behind, the smallest thing that fails if the logic breaks (an assert-based demo/self-check or one small test file; no frameworks, no fixtures). Trivial one-liners need no test."#;

pub fn system_prompt_fingerprint(options: &SystemPromptOptions) -> String {
    let shell = options
        .shell
        .as_ref()
        .map(|shell| format!("{:?}:{}", shell.kind, shell.program))
        .unwrap_or_default();
    let shape = json!({
        "cwd": options.cwd.trim_start_matches(r"\\?\").replace('\\', "/"),
        "readOnly": options.read_only,
        "fastContext": options.fast_context,
        "memoryEnabled": options.memory_enabled,
        "autoChangeProject": options.auto_change_project,
        "browser": options.browser,
        "shell": shell,
        "skills": options.skills_text,
        "customInstructions": options.custom_instructions,
        "ponytail": options.ponytail,

        "editMode": std::env::var("LYRA_EDIT_MODE").unwrap_or_default(),
    });
    let digest = Sha256::digest(serde_json::to_vec(&shape).unwrap_or_default());
    format!("{digest:x}")[..16].to_string()
}

/// 稳定段在前、动态段在后，配合 session 级 prompt_cache_key 最大化前缀缓存命中。
pub fn build_system_prompt(options: &SystemPromptOptions) -> String {
    let polaris = options.fast_context;
    let read_only = options.read_only;
    // 显式列出可用工具（按 Lyra 实际注册的工具集，只读模式无 bash/edit/write）。
    let tool_lines: Vec<&str> = [
        options.auto_change_project.then_some(
            "- change_working_directory: 切换后续工具根目录，并在 Nova 中切换或创建对应项目",
        ),
        Some("- read: 读取单个文件"),
        options
            .browser
            .then_some("- browser: 通过 Playwright 打开并调试网站、交互、查看前端错误与截图描述"),
        if read_only {
            None
        } else {
            Some(match &options.shell {
                Some(shell) if shell.kind == ShellKind::PowerShell => {
                    "- bash: 执行 PowerShell 命令"
                }
                _ => "- bash: 执行 Bash 命令",
            })
        },
        polaris.then_some(
            "- polaris: 一次打包完整编辑单元 + 依赖定义 + IMPACT/SIG（内部批量 rg + 增量符号索引）",
        ),
        if read_only {
            None
        } else {
            Some("- edit / write: 单文件编辑或写入")
        },
    ]
    .into_iter()
    .flatten()
    .collect();
    // feedback_memory 暂时禁用，不再出现在工具清单与提示词里。
    let _ = options.memory_enabled;
    let mut stable: Vec<String> = vec![
        "你是 Lyra：高效、简单、面向软件工程结果。".into(),
        format!("Available tools:\n{}", tool_lines.join("\n")),

        if polaris {
            "你拥有 Lyra 的原生 read、bash、edit、write 工具。以下工具选择规则是硬性约束。读取内容遵循最小必要原则：已知目标行范围时，只读取相关行段；需要更多上下文时再按需读取相邻行段。需要理解大文件整体结构时改用 polaris。任务涉及跨文件查找或修改（含分析要改哪里）时，先调用一次 polaris；一次调用通常替代 5–10 轮 rg+read 往返。拿不准是否涉及多个文件、或只是先分析要改哪里而不写代码时，同样按涉及处理，先调用 polaris。定位后仍需阅读两个及以上文件正文时，把文件清单传给 polaris 的 files 一次打包，不要逐个 read。已展示范围视为已读，SIG/IMPACT 仅在确需函数体时按 path:line 精确补读；大文件禁止无目的全量读取。修改已有文件时使用原生 edit；同一文件的多处修改必须合并进同一次 edit 调用的 edits 数组；多个互不依赖的文件可在同轮并行发起多个 edit 调用，但禁止对同一文件并发 edit；后续 edit 的 oldText 若依赖前一个 edit 写出的内容，必须等前者完成后再发起。已知多个独立路径时，同轮并行发多个 read。仅在存在先后依赖或目标重叠时串行调用工具。"
        } else {
            "你拥有 Lyra 的原生 read、bash、edit、write 工具。未知目标位置时，先用搜索工具定位行号，再读取命中位置附近的必要上下文；大文件禁止无目的全量读取。修改已有文件时使用原生 edit；同一文件的多处修改必须合并进同一次 edit 调用的 edits 数组；多个互不依赖的文件可在同轮并行发起多个 edit 调用，但禁止对同一文件并发 edit；后续 edit 的 oldText 若依赖前一个 edit 写出的内容，必须等前者完成后再发起。已知多个独立路径时，同轮并行发多个 read。仅在存在先后依赖或目标重叠时串行调用工具。"
        }
        .into(),
        if polaris {
            "搜索与遍历必须成本有界。路径和行段已明确且只需少量行段时直接 read；任务涉及跨文件查找或修改（含分析要改哪里）时，先调用一次 polaris（完整 EDIT/DEPS 单元 + IMPACT/SIG；内部批量 rg 与增量符号索引，一次调用通常替代 5–10 轮 rg+read 往返）。polaris 已展示范围视为已读；SIG/IMPACT 仅在确需函数体时精确补读。调用后不要对同一批关键词再用 bash 中的 `rg`/`git grep` 重复发现，也不要仅为查看更多内容放大预算重调；返回 CTX MISS 时按输出中的 next 提示修正符号名或用 files 指定入口文件重试一次，不要退回 rg/grep 逐个搜索。禁止使用 `grep -r` 或 `grep -R` 对仓库根目录或源码根目录进行无排除的递归搜索；兜底搜索默认遵守 `.gitignore`。除非任务明确要求，不得扫描构建产物、依赖、缓存、生成文件或大型二进制资源目录。`| head`、`| tail` 和输出截断只限制结果展示，不属于工作量限制；递归命令必须通过限定路径、glob、文件类型或排除目录缩小实际扫描范围，并设置较短的 timeout。递归命令超时后不得原样重试，必须缩小范围或改用更合适的搜索工具。"
        } else {
            "搜索与遍历必须成本有界。禁止使用 `grep -r` 或 `grep -R` 对仓库根目录或源码根目录进行无排除的递归搜索；优先使用 `rg`（遵守 `.gitignore`），仅在需要只搜已跟踪文件时回退 `git grep`。除非任务明确要求，不得扫描构建产物、依赖、缓存、生成文件或大型二进制资源目录。`| head`、`| tail` 和输出截断只限制结果展示，不属于工作量限制；递归命令必须通过限定路径、glob、文件类型或排除目录缩小实际扫描范围，并设置较短的 timeout。递归命令超时后不得原样重试，必须缩小范围或改用更合适的搜索工具。"
        }
        .into(),
        if options.auto_change_project { "需要切换仓库或子目录作为后续工具根目录时，使用 change_working_directory；成功后 Nova 会切换到已有项目，项目不存在则自动创建。该工具必须单独调用并等待成功，不能与依赖新目录的工具并行。" } else { "" }.into(),
        if options.memory_enabled { "polaris 会用 task 自动附带相关训练知识（task 为空时回退 keywords）。若结果含 TRAINED KNOWLEDGE，当前会话新事实优先；rule 是强约束，memory 是可核验事实，experience 仅在条件匹配时适用。" } else { "" }.into(),
        "先理解再修改，保持改动聚焦。".into(),
        if options.ponytail { PONYTAIL_RULES.into() } else { String::new() },
        "最终回复采用例外汇报，而不是完整工作报告。先直接给出用户可感知的结果；只有信息会影响结果判断、下一步行动、风险认知或可信度时，才写入最终回复。默认省略文件/函数/行号清单、搜索和工具调用过程、常规实现细节、具体测试命令、成功步骤清单、无实际影响的注意事项、泛化建议、‘无风险’声明，以及对同一结果的重复总结。正常成功时用 1～3 句话，不强制使用标题或列表；存在失败、未验证、行为变化、兼容性风险或用户必须操作的事项时，只围绕这些例外按需展开。用户明确询问实现细节时才提供详细报告。".into(),
        "完成修改后按需验证：改动小、影响明确时做成本最低的检查即可，影响面大或不确定时再按需扩大到受影响单元及直接使用方；禁止遍历或列出完整仓库、无依据扩大范围。最终回复只需说明验证是否通过，不列具体命令和逐项过程；仅当验证失败、无法验证或结果存在关键限制时补充原因与影响。".into(),
        match &options.shell {
            Some(shell) if shell.kind == ShellKind::PowerShell => format!(
                "命令终端已确认使用 PowerShell（{}）；bash 工具在 Windows 下通过 PowerShell 执行命令，必须从第一次调用起使用 PowerShell 语法（cmdlet、`;` 串联多条命令、`$env:NAME` 访问环境变量），不要使用 Bash 语法（`export`、`&&` 串联在 Windows PowerShell 5.1 中不可用、POSIX 风格的 sed/awk/grep 调用）。",
                shell.program
            ),
            Some(shell) => format!(
                "命令终端已确认使用 Bash（{}）；bash 工具必须从第一次调用起使用 Bash 语法，不要使用 PowerShell cmdlet。",
                shell.program
            ),
            None => String::new(),
        },
    ];
    stable.retain(|part| !part.is_empty());

    let mut dynamic: Vec<String> = Vec::new();
    if options.browser {
        dynamic.push("当前处于持续的浏览器调试模式：browser 工具与代码工具可联合使用，Playwright 标签页跨轮次保留。默认继续在现有页面调试，不要在每轮结束时 close；仅在用户发送 /browser-exit 或明确要求时关闭。".into());
    }
    if read_only {
        dynamic.push("当前为计划模式：只读分析，不得修改文件。".into());
    }
    // 统一为正斜杠并去掉 Windows 的 \\?\ 前缀。
    let cwd = options.cwd.trim_start_matches(r"\\?\").replace('\\', "/");
    dynamic.push(format!("Current working directory: {cwd}"));
    if !options.skills_text.is_empty() {
        dynamic.push(options.skills_text.clone());
    }
    if !options.custom_instructions.is_empty() {
        dynamic.push(options.custom_instructions.clone());
    }
    dynamic.retain(|part| !part.is_empty());

    format!("{}\n\n---\n\n{}", stable.join("\n\n"), dynamic.join("\n\n"))
}

pub struct SystemPromptOptions {
    pub cwd: String,
    pub read_only: bool,
    pub fast_context: bool,
    pub memory_enabled: bool,
    pub auto_change_project: bool,
    pub browser: bool,
    pub shell: Option<ShellConfig>,
    pub skills_text: String,
    pub custom_instructions: String,
    pub ponytail: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_change_project_controls_tool_prompt() {
        let options = SystemPromptOptions {
            cwd: "/tmp/project".into(),
            read_only: false,
            fast_context: false,
            memory_enabled: false,
            auto_change_project: false,
            browser: false,
            shell: None,
            skills_text: String::new(),
            custom_instructions: String::new(),
            ponytail: false,
        };
        let prompt = build_system_prompt(&options);
        assert!(!prompt.contains("change_working_directory"));
    }

    #[test]
    fn governs_oversized_text_with_head_tail() {
        let text = "a".repeat(100_000);
        let (governed, archive, original) = govern_tool_text(&text, 1_024, None, "call-1", "bash");
        assert!(governed.len() <= 1_024 + 128);
        assert!(governed.contains("[elided tool result — 100000 bytes]"));
        assert!(archive.is_none());
        assert_eq!(original, 100_000);
    }

    #[test]
    fn keeps_small_text() {
        let (governed, ..) = govern_tool_text("ok", 1_024, None, "c", "read");
        assert_eq!(governed, "ok");
    }

    #[test]
    fn clamps_cache_key() {
        assert_eq!(clamp_prompt_cache_key("  "), None);
        let long = "x".repeat(100);
        assert_eq!(clamp_prompt_cache_key(&long).unwrap().chars().count(), 64);
    }

    #[test]
    fn classifies_retryable() {
        assert!(is_retryable_provider_error("HTTP 429 too many requests"));
        assert!(is_retryable_provider_error(
            "Lyra provider 请求失败：HTTP 503 Service Unavailable"
        ));
        assert!(is_retryable_provider_error("connection error: ECONNRESET"));
        assert!(is_retryable_provider_error(
            "provider error: Upstream stream ended prematurely; safe to retry"
        ));
        assert!(is_retryable_provider_error(
            "读取响应流失败：error decoding response body"
        ));
        assert!(is_retryable_provider_error("provider SSE 连续 90s 无数据"));
        assert!(is_retryable_provider_error(
            "provider stream idle timeout: 90s 无增量事件"
        ));
        assert!(!is_retryable_provider_error("invalid api key"));
    }

    #[test]
    fn expands_browser_command_only_when_explicitly_invoked() {
        let expanded = expand_browser_command("/browser http://localhost:5173 检查登录页").unwrap();
        assert!(expanded.contains("browser open"));
        assert!(expanded.contains("不要在页面打开前扫描代码仓库"));
        assert!(expanded.contains("http://localhost:5173 检查登录页"));
        assert!(expand_browser_command("普通前端开发任务").is_none());
        assert!(expand_browser_command("/browsering test").is_none());
        assert!(is_browser_exit_command("/browser-exit"));
        assert!(!is_browser_exit_command("/browser-exit later"));
    }

    #[test]
    fn expands_skill_command() {
        let skills = vec![Skill {
            name: "deploy".into(),
            description: "部署".into(),
            path: PathBuf::from("/tmp/skills/deploy/SKILL.md"),
        }];
        let expanded = expand_skill_command("/skill:deploy 生产环境", &skills);
        assert!(expanded.contains("deploy"));
        assert!(expanded.contains("生产环境"));
        assert_eq!(expand_skill_command("普通输入", &skills), "普通输入");
    }
}

#[cfg(test)]
mod context_error_tests {
    use super::is_context_window_error;

    #[test]
    fn recognizes_provider_context_limit_errors() {
        assert!(is_context_window_error(
            "Your input exceeds the context window of this model."
        ));
        assert!(is_context_window_error(
            "invalid_request_error: context_length_exceeded"
        ));
        assert!(is_context_window_error("Prompt is too long"));
        assert!(!is_context_window_error("connection reset by peer"));
    }
}
