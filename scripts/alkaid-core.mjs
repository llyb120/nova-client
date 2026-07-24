import { Agent } from "@earendil-works/pi-agent-core";
import { streamSimple } from "@earendil-works/pi-ai/compat";
import {
  createCodingTools,
  createReadOnlyTools,
  formatSkillsForPrompt,
  getShellConfig,
  loadSkillsFromDir,
} from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { createReadStream, existsSync } from "node:fs";
import { readFile, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { delimiter, dirname, extname, join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { applySmartEdits } from "./alkaid-smart-edit.mjs";

const DEFAULT_BATCH_READ_LINES = 200;
/** Match pi coding tools: keep read_files outputs usable without blowing the context window. */
const READ_FILES_MAX_BYTES = 50 * 1024;
/** OpenAI Responses API hard limit for function_call_output.output string length. */
export const OPENAI_TOOL_OUTPUT_MAX_CHARS = 10_485_760;
/** Leave room for a truncation notice before the API rejects the request. */
export const OPENAI_TOOL_OUTPUT_SAFE_MAX_CHARS = OPENAI_TOOL_OUTPUT_MAX_CHARS - 512;
const DEFAULT_PROVIDER_RETRY_DELAYS_MS = [1000, 3000];
export const ALKAID_PROVIDER_IDLE_TIMEOUT_MS = 90_000;
const OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH = 64;
const SKILL_COMPRESSION_MIN_COUNT = 4;
const IMAGE_MEDIA_TYPES = {
  ".gif": "image/gif",
  ".jpeg": "image/jpeg",
  ".jpg": "image/jpeg",
  ".png": "image/png",
  ".webp": "image/webp",
};

const textResult = (text, details = undefined) => ({
  content: [{ type: "text", text: String(text) }],
  details,
});

/** Truncate oversized tool text so OpenAI accepts function_call_output.output / tool content. */
export function clampToolOutputText(text, maxChars = OPENAI_TOOL_OUTPUT_SAFE_MAX_CHARS) {
  const value = String(text ?? "");
  if (value.length <= maxChars) return value;
  const notice = `\n\n…[truncated: tool output exceeded ${maxChars} chars; original length ${value.length}]`;
  const keep = Math.max(0, maxChars - notice.length);
  return `${value.slice(0, keep)}${notice}`;
}

function truncateUtf8ToBytes(text, maxBytes) {
  if (Buffer.byteLength(text, "utf8") <= maxBytes) return text;
  let end = Math.min(text.length, maxBytes);
  let slice = text.slice(0, end);
  while (end > 0 && Buffer.byteLength(slice, "utf8") > maxBytes) {
    end = Math.floor(end * 0.9);
    slice = text.slice(0, end);
  }
  while (end < text.length && Buffer.byteLength(text.slice(0, end + 1), "utf8") <= maxBytes) {
    end += 1;
  }
  return text.slice(0, end);
}

/**
 * Clamp oversized tool outputs already present in an OpenAI request payload
 * (Responses `input[].output` or Completions `messages[].content` for role=tool).
 * Returns a new payload when anything changed; otherwise undefined.
 */
export function clampOpenAIPayloadToolOutputs(payload, maxChars = OPENAI_TOOL_OUTPUT_SAFE_MAX_CHARS) {
  if (!payload || typeof payload !== "object" || Array.isArray(payload)) return undefined;
  let changed = false;
  const next = { ...payload };

  if (Array.isArray(next.input)) {
    next.input = next.input.map((item) => {
      if (!item || typeof item !== "object" || item.type !== "function_call_output") return item;
      if (typeof item.output === "string" && item.output.length > maxChars) {
        changed = true;
        return { ...item, output: clampToolOutputText(item.output, maxChars) };
      }
      if (Array.isArray(item.output)) {
        let partsChanged = false;
        const output = item.output.map((part) => {
          if (part?.type === "input_text" && typeof part.text === "string" && part.text.length > maxChars) {
            partsChanged = true;
            return { ...part, text: clampToolOutputText(part.text, maxChars) };
          }
          return part;
        });
        if (partsChanged) {
          changed = true;
          return { ...item, output };
        }
      }
      return item;
    });
  }

  if (Array.isArray(next.messages)) {
    next.messages = next.messages.map((message) => {
      if (message?.role !== "tool") return message;
      if (typeof message.content === "string" && message.content.length > maxChars) {
        changed = true;
        return { ...message, content: clampToolOutputText(message.content, maxChars) };
      }
      return message;
    });
  }

  return changed ? next : undefined;
}

function resolveInputPath(root, input) {
  return resolve(root, input);
}

function resolveEditPath(root, input) {
  return resolveInputPath(root, input);
}

function alkaidDataRoot(home = homedir(), env = process.env) {
  return join(env.NOVA_DATA_DIR || join(home, ".nova"), "alkaid");
}

export function alkaidSkillsRoot(home = homedir(), env = process.env) {
  return join(alkaidDataRoot(home, env), "skills");
}

export async function alkaidPromptInput(parts = []) {
  const textParts = [];
  const images = [];
  for (const part of parts) {
    if (part.type === "text") textParts.push(part.text);
    if (part.type === "image_data") {
      images.push({ type: "image", data: part.data, mimeType: part.mime });
    }
    if (part.type === "local_image") {
      const mimeType = IMAGE_MEDIA_TYPES[extname(part.path).toLowerCase()];
      if (mimeType) {
        images.push({ type: "image", data: (await readFile(part.path)).toString("base64"), mimeType });
      } else {
        textParts.push(`Attached file: ${part.path}`);
      }
    }
  }
  return { text: textParts.join("\n\n"), images };
}

export async function alkaidUserMessage(parts = []) {
  const input = await alkaidPromptInput(parts);
  return {
    role: "user",
    content: [
      ...(input.text ? [{ type: "text", text: input.text }] : []),
      ...input.images,
    ],
    timestamp: Date.now(),
  };
}

export class AlkaidProviderIdleTimeoutError extends Error {
  constructor(timeoutMs) {
    super(`Vega provider stream idle timeout after ${timeoutMs}ms`);
    this.name = "AlkaidProviderIdleTimeoutError";
  }
}

export function isRetryableAlkaidProviderError(error) {
  if (error instanceof AlkaidProviderIdleTimeoutError) return true;
  const message = String(error ?? "").toLowerCase();
  return [
    "terminated",
    "fetch failed",
    "socket hang up",
    "econnreset",
    "etimedout",
    "econnaborted",
    "epipe",
    "und_err_socket",
    "premature close",
    "other side closed",
    "network connection lost",
    "idle timeout",
    "429",
    "too many requests",
    "rate limit",
  ].some((fragment) => message.includes(fragment));
}

export function createAlkaidIdleTimeout(options = {}) {
  const timeoutMs = options.timeoutMs ?? ALKAID_PROVIDER_IDLE_TIMEOUT_MS;
  const onTimeout = options.onTimeout ?? (() => {});
  let timer;
  let rejectTimeout;
  let active = false;
  let paused = false;

  function clearTimer() {
    if (timer !== undefined) clearTimeout(timer);
    timer = undefined;
  }

  function arm() {
    clearTimer();
    if (!active || paused || timeoutMs <= 0) return;
    timer = setTimeout(() => {
      timer = undefined;
      onTimeout();
      rejectTimeout?.(new AlkaidProviderIdleTimeoutError(timeoutMs));
    }, timeoutMs);
  }

  return {
    touch() {
      arm();
    },
    pause() {
      paused = true;
      clearTimer();
    },
    resume() {
      paused = false;
      arm();
    },
    async run(operation) {
      active = true;
      paused = false;
      const timeout = new Promise((_, reject) => {
        rejectTimeout = reject;
        arm();
      });
      try {
        return await Promise.race([operation(), timeout]);
      } finally {
        active = false;
        rejectTimeout = undefined;
        clearTimer();
      }
    },
  };
}

const wait = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

/** PI reports usage per model request, so a tool-using agent turn must sum every assistant message. */
export function mergeAlkaidUsage(total, usage) {
  if (!usage) return total;
  const merged = total ?? { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 };
  for (const key of ["input", "output", "cacheRead", "cacheWrite"]) {
    merged[key] += Number.isFinite(usage[key]) ? usage[key] : 0;
  }
  return merged;
}

export async function runAlkaidPromptWithRetry(agent, input, images, options = {}) {
  const retryDelaysMs = options.retryDelaysMs ?? DEFAULT_PROVIDER_RETRY_DELAYS_MS;
  const sleep = options.sleep ?? wait;
  const isCancelled = options.isCancelled ?? (() => false);
  const runAttempt = options.runAttempt ?? ((operation) => operation());
  let retries = 0;
  let operation = () => agent.prompt(input, images);

  while (true) {
    let thrownError;
    try {
      await runAttempt(operation);
    } catch (error) {
      thrownError = error;
      // The idle-timeout race rejects as soon as abort is requested, while PI may still be
      // appending its final aborted assistant message. Wait for that run to settle before
      // pruning it; otherwise the late message makes the following continue() fail because
      // the transcript ends in an assistant role.
      if (error instanceof AlkaidProviderIdleTimeoutError && typeof agent.waitForIdle === "function") {
        await agent.waitForIdle();
      }
    }

    const last = agent.state.messages.at(-1);
    const providerError = thrownError ?? (
      last?.role === "assistant" && last.stopReason === "error" ? last.errorMessage : undefined
    );
    if (!providerError) {
      return { last, retries, cancelled: last?.role === "assistant" && last.stopReason === "aborted" };
    }
    if (retries >= retryDelaysMs.length || !isRetryableAlkaidProviderError(providerError)) {
      if (thrownError) throw thrownError;
      return { last, retries, cancelled: false };
    }
    if (isCancelled()) return { last, retries, cancelled: true };
    if (thrownError instanceof AlkaidProviderIdleTimeoutError) {
      while (agent.state.messages.at(-1)?.role === "assistant") {
        agent.state.messages = agent.state.messages.slice(0, -1);
      }
    } else if (last?.role === "assistant") {
      agent.state.messages = agent.state.messages.slice(0, -1);
    }
    options.onRetry?.({ attempt: retries + 1, error: providerError });
    await sleep(retryDelaysMs[retries]);
    retries += 1;
    if (isCancelled()) return { last: agent.state.messages.at(-1), retries, cancelled: true };
    operation = () => agent.continue();
  }
}

async function readTextLines(path, offset = 1, limit = DEFAULT_BATCH_READ_LINES, maxBytes = READ_FILES_MAX_BYTES) {
  const input = createReadStream(path, { encoding: "utf8" });
  const lines = createInterface({ input, crlfDelay: Infinity });
  const content = [];
  let lineNumber = 0;
  let truncated = false;
  let byteCount = 0;
  try {
    for await (const line of lines) {
      lineNumber += 1;
      if (lineNumber < offset) continue;
      if (content.length === limit) {
        truncated = true;
        break;
      }
      const separatorBytes = content.length > 0 ? 1 : 0;
      const lineBytes = Buffer.byteLength(line, "utf8");
      if (byteCount + separatorBytes + lineBytes > maxBytes) {
        truncated = true;
        const remaining = maxBytes - byteCount - separatorBytes;
        if (remaining > 0) content.push(truncateUtf8ToBytes(line, remaining));
        break;
      }
      content.push(line);
      byteCount += separatorBytes + lineBytes;
    }
  } finally {
    lines.close();
    input.destroy();
  }
  return {
    content: content.join("\n"),
    truncated,
    nextOffset: truncated ? offset + Math.max(content.length, 1) : undefined,
  };
}

export function createFilesystemTools(cwd, editTool = null) {
  const root = resolve(cwd);
  const tools = [
    {
      name: "read_files",
      description: `同一读取阶段已有两个及以上路径已知、互不依赖的 UTF-8 文本目标时必须调用一次本工具，不得拆成多个 read；内部并行、流式读取，默认每个文件读取前 ${DEFAULT_BATCH_READ_LINES} 行（且不超过约 50KB）。请为每个文件按需指定 offset/limit，并用返回的 nextOffset 继续读取。`,
      parameters: Type.Object({
        paths: Type.Array(Type.Union([
          Type.String(),
          Type.Object({
            path: Type.String(),
            offset: Type.Optional(Type.Integer({ minimum: 1 })),
            limit: Type.Optional(Type.Integer({ minimum: 1, maximum: 2000 })),
          }),
        ]), { minItems: 1 }),
      }),
      async execute(_id, { paths }) {
        const results = await Promise.all(paths.map(async (input) => {
          const request = typeof input === "string" ? { path: input } : input;
          try {
            const path = resolveInputPath(root, request.path);
            const result = await readTextLines(path, request.offset, request.limit);
            return {
              path: request.path,
              content: result.content,
              ...(result.truncated ? { truncated: true, nextOffset: result.nextOffset } : {}),
            };
          } catch (error) {
            return { path: request.path, error: error instanceof Error ? error.message : String(error) };
          }
        }));
        return textResult(JSON.stringify(results), { count: results.length });
      },
    },
  ];
  if (editTool) {
    tools.push({
      name: "edit_files",
      description: "并行智能编辑多个互不依赖的文件。先精确匹配，再通过稀有行锚点按 rstrip、Unicode、相对缩进和保守模糊评分逐级定位；歧义或重叠时拒绝，所有文件验证成功后才并行写入。",
      parameters: Type.Object({
        files: Type.Array(Type.Object({
          path: Type.String(),
          edits: Type.Array(Type.Object({ oldText: Type.String(), newText: Type.String() }), { minItems: 1 }),
        }), { minItems: 1 }),
      }),
      async execute(_id, { files }, signal) {
        const grouped = new Map();
        for (const file of files) {
          const target = resolveEditPath(root, file.path);
          const existing = grouped.get(target);
          if (existing) existing.edits.push(...file.edits);
          else grouped.set(target, { path: file.path, target, edits: [...file.edits] });
        }
        const targets = [...grouped.values()];
        if (signal?.aborted) throw new Error("Operation aborted");

        // Read and locate every edit against immutable snapshots before writing any file.
        // Repeated path entries are one patch target; the patch algorithm decides whether
        // their edits are uniquely locatable and non-overlapping.
        const prepared = await Promise.all(targets.map(async (file) => {
          const raw = await readFile(file.target, "utf8");
          const bom = raw.startsWith("\uFEFF") ? "\uFEFF" : "";
          const withoutBom = bom ? raw.slice(1) : raw;
          const lineEnding = withoutBom.includes("\r\n") ? "\r\n" : "\n";
          const normalized = withoutBom.replace(/\r\n/g, "\n");
          const result = applySmartEdits(normalized, file.edits, file.path);
          return {
            path: file.path,
            target: file.target,
            original: raw,
            output: bom + (lineEnding === "\r\n" ? result.content.replace(/\n/g, "\r\n") : result.content),
            matches: result.matches,
          };
        }));
        if (signal?.aborted) throw new Error("Operation aborted");

        const writes = await Promise.allSettled(prepared.map((file) => writeFile(file.target, file.output, "utf8")));
        const failed = writes.findIndex((result) => result.status === "rejected");
        if (failed >= 0) {
          await Promise.allSettled(prepared.map((file, index) =>
            writes[index].status === "fulfilled" ? writeFile(file.target, file.original, "utf8") : Promise.resolve()));
          throw writes[failed].reason;
        }
        return textResult(`已并行智能编辑 ${prepared.length} 个文件`, {
          paths: prepared.map((file) => file.path),
          matches: prepared.map((file) => ({ path: file.path, edits: file.matches })),
        });
      },
    });
  }
  return tools;
}

/** Load skills via pi-coding-agent discovery (Agent Skills standard). */
export function loadAlkaidSkills(root = alkaidSkillsRoot()) {
  return loadSkillsFromDir({ dir: root, source: "user" });
}

function stripSkillFrontmatter(content) {
  if (!content.startsWith("---")) return content;
  const lines = content.split(/\r?\n/);
  if (lines[0].trim() !== "---") return content;
  const end = lines.slice(1).findIndex((line) => line.trim() === "---");
  return end < 0 ? content : lines.slice(end + 2).join("\n");
}

/** Expand pi-compatible /skill:<name> invocations before sending them to the model. */
export async function expandAlkaidSkillCommand(text, skills) {
  const match = String(text ?? "").match(/^\/skill:([^\s]+)(?:\s+([\s\S]*))?$/);
  if (!match) return text;
  const skill = skills.find((candidate) => candidate.name === match[1]);
  if (!skill) return text;
  try {
    const body = stripSkillFrontmatter(await readFile(skill.filePath, "utf8")).trim();
    const skillBlock = `<skill name="${skill.name}" location="${skill.filePath}">\nReferences are relative to ${skill.baseDir}.\n\n${body}\n</skill>`;
    const args = (match[2] ?? "").trim();
    return args ? `${skillBlock}\n\n${args}` : skillBlock;
  } catch {
    return text;
  }
}

function formatSkillsForPromptCompressed(skills) {
  const visible = skills.filter((skill) => !skill.disableModelInvocation);
  if (visible.length === 0) return "";
  const byRoot = new Map();
  for (const skill of visible) {
    const skillDir = dirname(skill.filePath);
    const root = dirname(skillDir).replace(/\\/g, "/");
    const list = byRoot.get(root) ?? [];
    list.push(skill.name);
    byRoot.set(root, list);
  }
  const lines = [
    "The following skills provide specialized instructions for specific tasks. When a skill name matches the task you are doing, read the SKILL.md at the listed location to load the full instructions. When a SKILL.md references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.",
  ];
  for (const root of [...byRoot.keys()].sort()) {
    const names = byRoot.get(root).slice().sort();
    lines.push(`Skills under ${root}/<name>/SKILL.md:`);
    lines.push(names.map((name) => `- ${name}`).join("\n"));
  }
  return lines.join("\n");
}

export function formatAlkaidSkillsPrompt(skills) {
  const visible = skills.filter((skill) => !skill.disableModelInvocation);
  if (visible.length === 0) return "";
  if (visible.length >= SKILL_COMPRESSION_MIN_COUNT) {
    return formatSkillsForPromptCompressed(skills);
  }
  return formatSkillsForPrompt(skills).trim();
}

export function optimizeAlkaidSystemPrompt(stableParts, dynamicParts) {
  const stable = stableParts.filter(Boolean).join("\n\n").trim();
  const dynamic = dynamicParts.filter(Boolean).join("\n\n").trim();
  if (!stable) return dynamic;
  if (!dynamic) return stable;
  return `${stable}\n\n---\n\n${dynamic}`;
}

export async function loadAlkaidAgentInstructions(path = join(alkaidDataRoot(), "AGENTS.md")) {
  return readFile(path, "utf8").catch((error) => {
    if (error?.code === "ENOENT") return "";
    throw new Error(`读取 Vega AGENTS.md 失败：${error instanceof Error ? error.message : String(error)}`);
  });
}

export function buildAlkaidSystemPrompt(options = {}) {
  const cwd = (options.cwd ?? process.cwd()).replace(/\\/g, "/");
  const skills = options.skills ?? [];
  const toolLines = [
    `- read_files: 并行读取多个 UTF-8 文本文件（可带 offset/limit）`,
    options.readOnly ? null : "- edit_files: 并行智能编辑多个互不依赖的已有文件（精确优先、锚点定位、歧义拒绝）",
    "- read: 读取单个文件",
    options.readOnly
      ? "- grep / find / ls: 只读搜索与列举"
      : options.shellConfig?.kind === "powershell"
        ? "- bash: 执行 PowerShell 命令"
        : "- bash: 执行 Bash 命令",
    options.readOnly ? null : "- edit / write: 单文件编辑或写入",
  ].filter(Boolean);

  const stableParts = [
    "你是 Vega：高效、简单、面向软件工程结果。",
    `Available tools:\n${toolLines.join("\n")}`,
    "你拥有批量增强 read_files、edit_files，以及 PI coding agent 的原生 read、bash、edit、write 工具。以下工具选择规则是硬性约束。每次准备读取前，先汇总当前已知目标：仅有一个目标时使用 read；同一读取阶段已有两个及以上路径已知、互不依赖的 UTF-8 文本目标时，必须在一次 read_files 调用中合并读取，并为每个文件分别设置必要的 offset/limit。禁止连续调用多个 read，也禁止用并行封装的多个 read 代替 read_files；想按顺序理解文件不构成读取依赖。只有后一个目标的路径或读取范围必须由前一次结果确定、目标不是 UTF-8 文本，或当前确实仅需一个文件时，才使用 read。后续新发现多个独立文本目标时，下一读取阶段仍须合并使用 read_files。读取内容遵循最小必要原则：已知目标行范围时，只读取相关行段；需要更多上下文时再按需读取相邻行段。未知目标位置时，先用搜索工具定位行号，再读取命中位置附近的必要上下文；大文件禁止无目的全量读取。修改两个及以上互不依赖的已有文件时必须使用 edit_files；同一文件的多处修改合并到该文件的一组 edits。仅在存在先后依赖或目标重叠时串行调用工具。",
    "搜索与遍历必须成本有界。禁止使用 `grep -r` 或 `grep -R` 对仓库根目录或源码根目录进行无排除的递归搜索；Git 仓库中搜索已跟踪文件时优先使用 `git grep`，需要搜索未跟踪文件时使用 `rg`，并默认遵守 `.gitignore`。除非任务明确要求，不得扫描构建产物、依赖、缓存、生成文件或大型二进制资源目录。`| head`、`| tail` 和输出截断只限制结果展示，不属于工作量限制；递归命令必须通过限定路径、glob、文件类型或排除目录缩小实际扫描范围，并设置较短的 timeout。递归命令超时后不得原样重试，必须缩小范围或改用更合适的搜索工具。",
    "先理解再修改，保持改动聚焦；完成后简洁报告结果和验证。",
    "完成修改后，优先根据版本控制 diff 按需确定受影响单元及直接使用方，并执行成本最低且有效的验证；禁止遍历或列出完整仓库、无依据扩大范围，纯文档类改动可说明依据后跳过测试，无法验证时须报告原因、建议命令及剩余风险。",
    options.shellConfig
      ? options.shellConfig.kind === "powershell"
        ? `命令终端已确认使用 PowerShell（${options.shellConfig.shell}）；bash 工具在 Windows 下通过 PowerShell 执行命令，必须从第一次调用起使用 PowerShell 语法（cmdlet、\`;\` 串联多条命令、\`$env:NAME\` 访问环境变量），不要使用 Bash 语法（\`export\`、\`&&\` 串联在 Windows PowerShell 5.1 中不可用、POSIX 风格的 sed/awk/grep 调用）。`
        : `命令终端已确认使用 Bash（${options.shellConfig.shell}）；bash 工具必须从第一次调用起使用 Bash 语法，不要使用 PowerShell cmdlet。`
      : "",
  ];

  const dynamicParts = [
    options.readOnly ? "当前为计划模式：只读分析，不得修改文件。" : "",
    `Current working directory: ${cwd}`,
    formatAlkaidSkillsPrompt(skills),
    options.systemPrompt ?? "",
  ];

  return optimizeAlkaidSystemPrompt(stableParts, dynamicParts);
}

export function clampPromptCacheKey(key) {
  const normalized = key?.trim();
  if (!normalized) return undefined;
  const chars = Array.from(normalized);
  if (chars.length <= OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH) return normalized;
  return chars.slice(0, OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH).join("");
}

export function injectOpenAIPromptCacheKey(payload, sessionId) {
  if (!payload || typeof payload !== "object" || Array.isArray(payload)) return undefined;
  const record = payload;
  if (typeof record.prompt_cache_key === "string" && record.prompt_cache_key.trim()) return undefined;
  if (typeof record.promptCacheKey === "string" && record.promptCacheKey.trim()) return undefined;
  const key = clampPromptCacheKey(sessionId);
  if (!key) return undefined;
  return { ...record, prompt_cache_key: key };
}

function createAlkaidStreamFn() {
  return (model, context, options = {}) => streamSimple(model, context, {
    ...options,
    cacheRetention: options.cacheRetention ?? "long",
  });
}

// Windows 下依次尝试：System32 自带的 Windows PowerShell → PATH 上的 powershell.exe。
export function findWindowsPowerShell(env = process.env) {
  const roots = [env.SystemRoot, env.windir].filter(Boolean);
  for (const root of roots) {
    const candidate = join(root, "System32", "WindowsPowerShell", "v1.0", "powershell.exe");
    if (existsSync(candidate)) return candidate;
  }
  const pathEntry = Object.entries(env).find(([key]) => key.toLowerCase() === "path");
  for (const dir of (pathEntry?.[1] ?? "").split(delimiter).filter(Boolean)) {
    const candidate = join(dir, "powershell.exe");
    if (existsSync(candidate)) return candidate;
  }
  return null;
}

// Vega 默认 shell 探测：Windows 直接使用 PowerShell（不再依赖 Git Bash），
// 找不到 PowerShell 时兜底回退到 pi 的 bash 探测；其他平台维持 bash。
export function detectAlkaidShellConfig(env = process.env, platform = process.platform) {
  if (platform !== "win32") return getShellConfig();
  const shell = findWindowsPowerShell(env);
  return shell ? { shell, args: ["-c"], kind: "powershell" } : getShellConfig();
}

export function resolveAlkaidShellConfig(shellConfig, env = process.env, platform = process.platform) {
  if (!shellConfig || platform !== "win32") return shellConfig;
  const shim = shellConfig.kind === "powershell"
    ? env.NOVA_SHELL_SHIM_POWERSHELL
    : env.NOVA_SHELL_SHIM_BASH;
  return shim ? { ...shellConfig, shell: shim } : shellConfig;
}

function mcpResult(result) {
  const content = (result.content ?? []).flatMap((part) => {
    if (part.type === "text") return [{ type: "text", text: clampToolOutputText(part.text) }];
    if (part.type === "image") return [{ type: "image", data: part.data, mimeType: part.mimeType }];
    return [{ type: "text", text: clampToolOutputText(JSON.stringify(part)) }];
  });
  return { content: content.length ? content : [{ type: "text", text: "MCP 工具执行完成" }], details: result };
}

export async function connectMcpServers(servers = {}, cwd = process.cwd()) {
  const connections = await Promise.all(Object.entries(servers).map(async ([serverName, config]) => {
    if (!config?.command) throw new Error(`MCP ${serverName} 缺少 command`);
    const client = new Client({ name: "alkaid", version: "0.1.0" });
    const transport = new StdioClientTransport({
      command: config.command,
      args: config.args ?? [],
      cwd,
      env: config.env ? { ...process.env, ...config.env } : undefined,
      stderr: "pipe",
    });
    await client.connect(transport);
    const listed = await client.listTools();
    const tools = listed.tools.map((tool) => ({
      name: `mcp__${serverName}__${tool.name}`,
      description: tool.description ?? `MCP ${serverName} / ${tool.name}`,
      parameters: tool.inputSchema,
      async execute(_id, params) {
        return mcpResult(await client.callTool({ name: tool.name, arguments: params }));
      },
    }));
    return { client, transport, tools };
  }));
  return {
    tools: connections.flatMap((connection) => connection.tools),
    async close() {
      await Promise.allSettled(connections.map((connection) => connection.transport.close()));
    },
  };
}

export async function createAlkaidAgent(options = {}) {
  if (!options.model) throw new Error("Vega 缺少模型配置");
  const cwd = resolve(options.cwd ?? process.cwd());
  const { skills } = loadAlkaidSkills(options.skillsRoot ?? alkaidSkillsRoot());
  const mcp = await connectMcpServers(options.mcpServers, cwd);
  const detectedShellConfig = options.readOnly ? null : (options.shellConfig ?? detectAlkaidShellConfig());
  const shellConfig = detectedShellConfig && resolveAlkaidShellConfig(detectedShellConfig);
  const codingTools = options.readOnly
    ? createReadOnlyTools(cwd)
    : createCodingTools(cwd, { bash: { shellPath: shellConfig.shell } });
  const editTool = codingTools.find((tool) => tool.name === "edit");
  const batchTools = createFilesystemTools(cwd, editTool);
  const tools = [...batchTools, ...codingTools, ...mcp.tools];
  const agentInstructions = await loadAlkaidAgentInstructions(options.agentInstructionsPath);
  const customInstructions = [agentInstructions.trim(), options.systemPrompt?.trim()]
    .filter(Boolean)
    .join("\n\n");
  const systemPrompt = buildAlkaidSystemPrompt({
    cwd,
    skills,
    readOnly: options.readOnly,
    shellConfig,
    systemPrompt: customInstructions,
  });
  const sessionId = options.sessionId;
  const api = options.model.api;
  const agent = new Agent({
    initialState: {
      systemPrompt,
      model: options.model,
      thinkingLevel: options.thinkingLevel,
      tools,
      messages: options.messages ?? [],
    },
    getApiKey: () => options.apiKey,
    streamFn: createAlkaidStreamFn(),
    toolExecution: "parallel",
    steeringMode: "all",
    sessionId,
    onPayload: (payload, model) => {
      const modelApi = model?.api ?? api;
      if (modelApi !== "openai-completions" && modelApi !== "openai-responses") return undefined;
      let next = payload;
      let changed = false;
      const withCache = injectOpenAIPromptCacheKey(next, sessionId);
      if (withCache) {
        next = withCache;
        changed = true;
      }
      const clamped = clampOpenAIPayloadToolOutputs(next);
      if (clamped) {
        next = clamped;
        changed = true;
      }
      return changed ? next : undefined;
    },
  });
  return { agent, close: () => mcp.close(), skills, toolCount: tools.length };
}
