//! 系统提示（稳定/动态分段，配合 prompt_cache_key 命中前缀缓存）、技能加载、
//! 工具结果治理（超限归档 + 首尾截断）、provider 重试判定与用量合并。

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Reasonix 风格的单工具上下文预算；超限文本先归档再截断。
pub const TOOL_OUTPUT_CONTEXT_MAX_BYTES: usize = 32 * 1024;
/// polaris 默认快速概览有自己的输出预算（默认 12KB，显式上限 64KB）。
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
        "gif" => Some("image/gif"),
        "jpeg" | "jpg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "webp" => Some("image/webp"),
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
    "upstream stream ended before terminal chunk",
    "safe to retry",
    "stream ended before a terminal response event",
    "stream ended without finish_reason",
    "provider finish_reason",
    "provider stop_reason",
    "provider returned no actionable output",
    "error decoding response body",
    "idle timeout",
    "sse 连续",
    "429",
    "too many requests",
    "rate limit",
    "503",
    "service unavailable",
    "no available accounts",
    "please retry later",
    "timed out",
    "error sending request",
];

pub fn is_retryable_provider_error(error: &str) -> bool {
    let message = error.to_lowercase();
    // refusal 是模型确定性拒答，重试同一请求不会变；交给 scene fallback 或直接结束本轮。
    !is_context_window_error(error)
        && !is_output_length_error(error)
        && !message.contains("stop_reason: refusal")
        && RETRYABLE_FRAGMENTS
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
        "request_too_large",
        "payload too large",
    ]
    .iter()
    .any(|fragment| message.contains(fragment))
        // k3 等 provider 的超限文案形如 “k3-256k supports only 256K context.”。
        || (message.contains("supports only") && message.contains("context"))
}

/// 输出耗尽不等于输入超限；须调整思考预算，不能走原样重试。
pub fn is_output_length_error(error: &str) -> bool {
    let message = error.to_lowercase();
    message.contains("no actionable output") && message.contains("stop_reason=length")
}

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
/// 同步 DietrichGebert/ponytail v5.3.2 的 AGENTS.md；档位来自 skills/ponytail/SKILL.md。
pub const PONYTAIL_RULES: &str = r#"# Ponytail, lazy senior dev mode

You are a lazy senior developer. The best code is the code never written. You solve the whole problem with the least new code. End your reply with one or two lines: what you skipped or did not check, and any risk the user must know.

## Before you write

Read the task and the code it touches. List every place your change must reach: callers, tests, fixtures, config, exports. Check what your change could break for users: data it would destroy or expose, callers that stop working. That is scope. Extra features are not.

## The smallest complete change

Take the first option that fully works:

1. Does it need to exist? Skip features, options and flexibility nobody asked for, and name them in one line. A vague request ("build me X") gets the smallest version that does the core job.
2. Already in this codebase (a helper, component, service, pattern)? Use it the way the surrounding code does.
3. Standard library or a platform feature? Use it, unless the project has its own. A house component beats a native widget.
4. An installed dependency? Use it. Never add a dependency for a few lines.
5. Can it be one line a reader gets at a glance? One line.
6. Otherwise: the minimum code that works.

- Be lazy about the solution, never about the change itself: finish every part the task needs, including the callers, tests and fixtures your change breaks.
- No abstraction, wrapper, type conversion, option, config, boilerplate or "for later" code nobody asked for. Keep values in the form the platform already gives you. Deletion beats addition. Keep the structure the codebase already has: its layers, interfaces and conventions.
- The shortest working diff wins, once you know everything it must touch. A one-liner that needs decoding is not short.
- Comment only the why the code cannot show, in one line.
- Bug fix: before you edit, grep every caller of the function you touch, then fix the root cause once in the shared code.
- Code you move or merge keeps its error handling and validation.
- Between options of equal size, take the one that is correct on edge cases.
- Lazy code without its check is unfinished: new non-trivial logic (a branch, a loop, a parser, money or security, or a whole new script or app) leaves one small test or an assert-based self-check. Trivial changes need none.
- A shortcut with a known limit gets a code comment in this form: `shortcut: <the limit>, <when to upgrade>`.

Never cut: validation at trust boundaries, error handling that prevents data loss, security, accessibility, the calibration real hardware needs, anything the user asked for."#;

pub const PONYTAIL_LEVELS: &str = r#"## Levels

| Level | Behavior |
|-------|----------|
| **lite** | Build what was asked. Name the smaller option in one line and let the user pick. |
| **full** | The rules above. Default. |
| **ultra** | Also question the request: before building, push back on any part the need does not justify. |"#;

pub fn ponytail_prompt(enabled: bool, level: &str) -> String {
    if !enabled || level == "off" {
        return String::new();
    }
    let level = match level {
        "lite" | "ultra" => level,
        _ => "full",
    };
    format!("{PONYTAIL_RULES}\n\n{PONYTAIL_LEVELS}\n\nActive level: {level}.")
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
        Some("- read: 读取单个文件（图片直接返回图像）"),
        (!read_only && options.desktop).then_some("- jianlai（剑来）: 仅在必须操作图形界面时用真实鼠标键盘与截图操作电脑；本地文件/图片用 read，命令能拿到的用 bash，Chrome 网页用 chrome"),
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
            "- polaris: 默认快速概览，每处1–2行命中+必要签名/定位及 IMPACT/SIG；不递归依赖、不追踪别名调用或 Git 历史，正文按需 read 补读；仅确需深度关系时传 deep=true（较慢）；默认12KB硬上限、不凑满，不自动展开完整函数",
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
    let ponytail = ponytail_prompt(options.ponytail, &options.ponytail_level);
    let mut stable: Vec<String> = vec![
        format!("Available tools:\n{}", tool_lines.join("\n")),

        if polaris {
            "读取与搜索（硬性约束）：\n- 已知路径和行段时直接 read 相关行段，需要时再读相邻行段；大文件禁止无目的全量读取，需要整体结构时用 polaris。\n- 跨文件查找或修改（含分析要改哪里）且现有上下文不足时，先调用一次 polaris（一次通常替代 5–10 轮 rg+read），优先传用户明确提到的模块名、符号和文件路径，不堆叠猜测的泛词；已展示且未失效的上下文足够时直接回答或修改，不重复检索。\n- 定位后仍需读两个及以上文件正文时，把文件清单传给 polaris 的 files 一次打包，不要逐个 read。\n- polaris 仅实际展示的行段视为已读；依赖/SIG/IMPACT 仅在确需正文时按 path:line 精确补读。\n- 调用 polaris 后不要对同一批关键词再用 `rg`/`git grep` 重复发现，也不要仅为看更多内容放大预算重调；返回 CTX MISS 时按 next 提示修正符号名或用 files 指定入口文件重试一次，不要退回 rg/grep 逐个搜索。"
        } else {
            "读取与搜索（硬性约束）：\n- 未知目标位置时先用 `rg`（遵守 `.gitignore`；只搜已跟踪文件时用 `git grep`）定位行号，再读命中位置附近的必要上下文；大文件禁止无目的全量读取。"
        }
        .into(),
        "- 搜索与遍历必须成本有界：禁止 `grep -r`/`grep -R` 对仓库或源码根目录无排除递归；除非任务明确要求，不扫描构建产物、依赖、缓存、生成文件或大型二进制资源目录。\n- `| head`、`| tail` 和输出截断只限制展示，不限制工作量；递归命令须限定路径、glob、文件类型或排除目录并设较短 timeout；超时后不得原样重试，须缩小范围或换更合适的工具。".into(),
        if read_only {
            String::new()
        } else {
            format!(
                "修改文件：\n- 修改已有文件用 edit；同一文件的多处修改合并进一次 edit 的 edits 数组，禁止对同一文件并发 edit；不同文件可同轮并行 edit。\n- oldText 能引用对话中已有内容（{}、上次 edit 返回的 diff、用户消息原文）时直接 edit，不先 read；edit 是原子操作，失败报错会给出最接近的真实内容与行号，据此修正一次。\n- edit 结果已含 diff 与变更后内容，禁止为确认改动重复 read 同一文件。",
                if polaris { "polaris 已展示内容" } else { "搜索结果" }
            )
        },
        if options.auto_change_project { "需要切换仓库或子目录作为后续工具根目录时，使用 change_working_directory；成功后 Nova 会切换到已有项目，项目不存在则自动创建。该工具必须单独调用并等待成功，不能与依赖新目录的工具并行。" } else { "" }.into(),
        "先理解再修改，保持改动聚焦。".into(),
        "减少模型往返、真正并发：\n- 每轮先找出所有参数已知的工作，一次提交尽可能多的工具调用（运行时最多同时执行 4 项）；只有下一步参数依赖未知结果或工具要求单独调用时才回模型决定，不猜参数、不扩大授权、不为凑批次造工作。\n- 互不依赖的调用分别发出（如多个独立路径同轮并行 read），不要用分号或 && 合成一条串行命令冒充并行。\n- 有顺序要求的步骤也同轮提交，用可选 _after:[序号] 声明必须先成功的本轮调用（从 1 计数，只能引用更早项）；前置失败会跳过后继，无关分支继续。例：第1项长任务、第2项短任务、第3项依赖第2项，则只给第3项 _after:[2]，它在第2项完成后立即启动。\n- 命令或 edit 会改文件时，依赖其结果的读取、验证或后续 edit 必须显式 _after 它；共用输出目录、数据库或其他资源的操作不算独立。\n- 界面工具和 MCP 工具的返回不等于业务成功，不要用 _after 自动推进其后继，先回读结果确认；已确定的连续界面步骤用界面工具的 run 操作。\n- 失败只处理失败分支，不重放成功项。".into(),
        // ponytail 开启时其“末尾写跳过项与风险”优先，汇报规则不再禁止这类声明。
        format!(
            "最终回复采用例外汇报，而不是完整工作报告。先直接给出用户可感知的结果；只有信息会影响结果判断、下一步行动、风险认知或可信度时，才写入最终回复。默认省略文件/函数/行号清单、搜索和工具调用过程、常规实现细节、具体测试命令、成功步骤清单、无实际影响的注意事项、泛化建议{}，以及对同一结果的重复总结。正常成功时用 1～3 句话，不强制使用标题或列表；存在失败、未验证、行为变化、兼容性风险或用户必须操作的事项时，只围绕这些例外按需展开。用户明确询问实现细节时才提供详细报告。{}",
            if ponytail.is_empty() { "、‘无风险’声明" } else { "" },
            if ponytail.is_empty() { "" } else { "结尾按 ponytail 规则用一两行写明跳过或未检查的内容及用户须知的风险；与本条冲突时以 ponytail 为准。" }
        ),
        "回复中的图片使用 Markdown 内联展示：`![图片说明](<图片地址>)`，不要放在代码块里，也不要只给普通文件链接。本地图片使用已确认存在的绝对路径（Windows 用正斜杠）；网络图片使用完整的 http(s) URL；地址用尖括号包裹以兼容空格。优先沿用工具返回的图片 Markdown，不编造图片路径或 URL。".into(),
        ponytail,
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
    pub auto_change_project: bool,
    pub shell: Option<ShellConfig>,
    pub skills_text: String,
    pub custom_instructions: String,
    pub ponytail: bool,
    pub ponytail_level: String,
    /// 自动模式非剑来场景不提供桌面工具。
    pub desktop: bool,
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
            auto_change_project: false,
            shell: None,
            skills_text: String::new(),
            custom_instructions: String::new(),
            ponytail: false,
            ponytail_level: "full".into(),
            desktop: false,
        };
        let prompt = build_system_prompt(&options);
        assert!(!prompt.contains("change_working_directory"));
        assert!(!prompt.contains("jianlai"));
        assert!(!prompt.contains("- parallel:"));
        assert!(prompt.contains("减少模型往返") && prompt.contains("_after:[2]"));
        assert!(!prompt.contains("你是 Lyra") && !prompt.contains("exec_command"));
        assert!(prompt.contains("‘无风险’声明") && !prompt.contains("Ponytail"));
        assert!(prompt.contains("![图片说明](<图片地址>)") && prompt.contains("绝对路径"));
        let prompt = build_system_prompt(&SystemPromptOptions { ponytail: true, ..options });
        assert!(!prompt.contains("‘无风险’声明") && prompt.contains("以 ponytail 为准"));
    }

    #[test]
    fn ponytail_modes_are_applied() {
        let full = ponytail_prompt(true, "full");
        assert!(full.contains("## The smallest complete change"));
        assert!(full.ends_with("Active level: full."));
        assert!(ponytail_prompt(true, "lite").ends_with("Active level: lite."));
        assert!(ponytail_prompt(true, "ultra").ends_with("Active level: ultra."));
        assert_eq!(ponytail_prompt(true, "unexpected"), full);
        assert_eq!(ponytail_prompt(true, "off"), "");
        assert_eq!(ponytail_prompt(false, "full"), "");
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
        assert!(is_retryable_provider_error(
            "Lyra provider 请求失败：Anthropic 流错误：No available accounts, please retry later"
        ));
        assert!(is_retryable_provider_error("connection error: ECONNRESET"));
        assert!(is_retryable_provider_error(
            "provider error: Upstream stream ended prematurely; safe to retry"
        ));
        assert!(is_retryable_provider_error(
            "provider 错误：Upstream stream ended before terminal chunk"
        ));
        assert!(is_retryable_provider_error(
            "读取响应流失败：error decoding response body"
        ));
        assert!(is_retryable_provider_error("provider SSE 连续 90s 无数据"));
        assert!(is_retryable_provider_error("provider finish_reason: other"));
        assert!(!is_retryable_provider_error("provider stop_reason: refusal"));
        assert!(is_retryable_provider_error(
            "provider stream idle timeout: 90s 无增量事件"
        ));
        assert!(!is_retryable_provider_error("invalid api key"));
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
    use super::{is_context_window_error, is_output_length_error, is_retryable_provider_error};

    #[test]
    fn recognizes_provider_context_limit_errors() {
        assert!(is_context_window_error(
            "Your input exceeds the context window of this model."
        ));
        assert!(is_context_window_error(
            "invalid_request_error: context_length_exceeded"
        ));
        assert!(is_context_window_error("Prompt is too long"));
        assert!(is_context_window_error(
            "HTTP 413 Payload Too Large：{\"error\":{\"code\":\"request_too_large\",\"message\":\"请求体过大\"}}"
        ));
        assert!(is_context_window_error(
            "HTTP 401 Unauthorized: {\"error\":{\"message\":\"k3-256k supports only 256K context.\",\"type\":\"invalid_authentication_error\"}}"
        ));
        assert!(!is_context_window_error("connection reset by peer"));
        let squeezed = "provider returned no actionable output: 模型未返回正文或工具调用（可能仅有思考内容，stop_reason=length）";
        assert!(!is_context_window_error(squeezed) && !is_retryable_provider_error(squeezed));
        assert!(is_output_length_error(squeezed));
        assert!(!is_output_length_error("provider stop_reason=length"));
        assert!(!is_output_length_error(&squeezed.replace("length", "stop")));
        assert!(is_retryable_provider_error(&squeezed.replace("length", "stop")));
        assert!(!is_context_window_error("this model supports only text input"));
    }
}
