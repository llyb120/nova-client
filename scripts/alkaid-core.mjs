import { Agent } from "../node_modules/@earendil-works/pi-agent-core/dist/agent.js";
import { streamSimple } from "@earendil-works/pi-ai/compat";
import {
  createCodingTools,
  createReadOnlyTools,
} from "../node_modules/@earendil-works/pi-coding-agent/dist/core/tools/index.js";
import {
  formatSkillsForPrompt,
  loadSkillsFromDir,
} from "../node_modules/@earendil-works/pi-coding-agent/dist/core/skills.js";
import { getShellConfig } from "../node_modules/@earendil-works/pi-coding-agent/dist/utils/shell.js";
import { Type } from "typebox";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { existsSync } from "node:fs";
import { access, mkdir, readFile, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { delimiter, dirname, extname, join, resolve } from "node:path";
import { findSymbols, FAST_CONTEXT_DESCRIPTION } from "./ctx-core.mjs";
import { callNapiTool } from "./nova-napi-tools.mjs";
import { callContextToolOrLocal } from "./nova-context-client.mjs";

/** Reasonix-style per-tool context budget. Full oversized text is archived before truncation. */
export const TOOL_OUTPUT_CONTEXT_MAX_BYTES = 32 * 1024;
/** fast_context has its own complete-unit budget (default 32KB, explicit max 64KB). */
export const FAST_CONTEXT_OUTPUT_MAX_BYTES = 64 * 1024;
/** OpenAI Responses API hard limit for function_call_output.output string length. */
export const OPENAI_TOOL_OUTPUT_MAX_CHARS = 10_485_760;
/** Leave room for a truncation notice before the API rejects the request. */
export const OPENAI_TOOL_OUTPUT_SAFE_MAX_CHARS = OPENAI_TOOL_OUTPUT_MAX_CHARS - 512;
const DEFAULT_PROVIDER_RETRY_DELAYS_MS = [1000, 3000];
export const ALKAID_PROVIDER_IDLE_TIMEOUT_MS = 120_000;
// Temporarily disabled: keep the timeout implementation available for controlled re-enablement.
export const ALKAID_PROVIDER_IDLE_TIMEOUT_ENABLED = false;
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

function safeArchiveSegment(value) {
  return String(value ?? "tool").replace(/[^A-Za-z0-9_.-]+/g, "-").slice(0, 96) || "tool";
}

function truncateUtf8TailToBytes(text, maxBytes) {
  if (Buffer.byteLength(text, "utf8") <= maxBytes) return text;
  let start = Math.max(0, text.length - maxBytes);
  let slice = text.slice(start);
  while (start < text.length && Buffer.byteLength(slice, "utf8") > maxBytes) {
    start += Math.max(1, Math.ceil((slice.length * 0.1)));
    slice = text.slice(start);
  }
  while (start > 0 && Buffer.byteLength(text.slice(start - 1), "utf8") <= maxBytes) start -= 1;
  return text.slice(start);
}

function headTailUtf8(text, maxBytes, notice) {
  const budget = Math.max(0, maxBytes - Buffer.byteLength(notice, "utf8"));
  const headBudget = Math.ceil(budget * 0.6);
  const tailBudget = Math.max(0, budget - headBudget);
  return `${truncateUtf8ToBytes(text, headBudget)}${notice}${truncateUtf8TailToBytes(text, tailBudget)}`;
}

/** Archive oversized text tool output and return a stable head/tail placeholder for model context. */
export async function governToolResult(result, options = {}) {
  const maxBytes = options.maxBytes ?? TOOL_OUTPUT_CONTEXT_MAX_BYTES;
  if (!result || !Array.isArray(result.content)) return result;
  const text = result.content
    .filter((part) => part?.type === "text")
    .map((part) => String(part.text ?? ""))
    .join("\n");
  const originalBytes = Buffer.byteLength(text, "utf8");
  if (originalBytes <= maxBytes) return result;

  let archivePath;
  if (options.archiveDir) {
    await mkdir(options.archiveDir, { recursive: true });
    archivePath = join(
      options.archiveDir,
      `${safeArchiveSegment(options.toolCallId)}-${safeArchiveSegment(options.toolName)}.txt`,
    );
    await writeFile(archivePath, text, "utf8");
  }
  const location = archivePath ? ` archived at ${archivePath}; use read with offset/limit to inspect it` : "";
  const notice = `\n\n…[elided tool result — ${originalBytes} bytes${location}]\n\n`;
  const governed = headTailUtf8(text, maxBytes, notice);
  return {
    ...result,
    content: [
      { type: "text", text: governed },
      ...result.content.filter((part) => part?.type !== "text"),
    ],
    details: {
      ...(result.details && typeof result.details === "object" ? result.details : {}),
      archivedToolOutput: archivePath,
      originalBytes,
    },
  };
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

export function messagesWithPendingAlkaidPrompt(messages, input, timestamp = Date.now()) {
  return [
    ...structuredClone(messages ?? []),
    {
      role: "user",
      content: [
        ...(input.text ? [{ type: "text", text: input.text }] : []),
        ...(input.images ?? []),
      ],
      timestamp,
    },
  ];
}

export async function alkaidUserMessage(parts = []) {
  const input = await alkaidPromptInput(parts);
  return messagesWithPendingAlkaidPrompt([], input)[0];
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
    "connection error",
    "socket hang up",
    "econnreset",
    "etimedout",
    "econnaborted",
    "epipe",
    "request timed out",
    "und_err_socket",
    "premature close",
    "other side closed",
    "network connection lost",
    "stream ended before a terminal response event",
    "stream ended without finish_reason",
    "idle timeout",
    "429",
    "too many requests",
    "rate limit",
  ].some((fragment) => message.includes(fragment));
}

export function restoreAlkaidSteeringForRetry(agent, steeringMessages = []) {
  const transcript = new Set(agent.state.messages);
  // Rebuild PI's private steering queue from bridge-owned messages. A prompt already injected
  // into the transcript stays there; a prompt still waiting in the old run is queued again.
  // This makes timeout recovery independent of queue state left behind by the aborted run.
  agent.clearSteeringQueue?.();
  let restored = 0;
  for (const message of steeringMessages) {
    if (transcript.has(message)) continue;
    agent.steer(message);
    restored += 1;
  }
  return restored;
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
      const cancelled = last?.role === "assistant" && last.stopReason === "aborted";
      // A steer can arrive after PI's final queue poll but before the run settles. Give the
      // bridge reader a short quiescence window, then drain anything queued instead of ending
      // the turn and silently orphaning the user's latest prompt. This is especially likely
      // around an idle-timeout retry because the turn stays open across multiple provider runs.
      if (!cancelled && !isCancelled()) {
        await options.settlePendingInput?.();
        if (typeof agent.hasQueuedMessages === "function" && agent.hasQueuedMessages()) {
          operation = () => agent.continue();
          continue;
        }
      }
      return { last, retries, cancelled };
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
    // The aborted provider run may have consumed a steer into the transcript or left it in
    // PI's private queue. Let the bridge reconstruct that state before continue() starts.
    await options.prepareRetry?.({ attempt: retries + 1, error: providerError });
    options.onRetry?.({ attempt: retries + 1, error: providerError });
    await sleep(retryDelaysMs[retries]);
    retries += 1;
    if (isCancelled()) return { last: agent.state.messages.at(-1), retries, cancelled: true };
    operation = () => agent.continue();
  }
}

function swapUtf16Bytes(buffer) {
  const swapped = Buffer.from(buffer);
  for (let i = 0; i + 1 < swapped.length; i += 2) {
    [swapped[i], swapped[i + 1]] = [swapped[i + 1], swapped[i]];
  }
  return swapped;
}

function detectTextEncoding(buffer) {
  if (buffer[0] === 0xff && buffer[1] === 0xfe) return { encoding: "utf16le", bomBytes: 2 };
  if (buffer[0] === 0xfe && buffer[1] === 0xff) return { encoding: "utf16be", bomBytes: 2 };
  if (buffer[0] === 0xef && buffer[1] === 0xbb && buffer[2] === 0xbf) return { encoding: "utf8", bomBytes: 3 };

  // Infer BOM-less UTF-16 only when NUL bytes strongly alternate. This avoids
  // treating ordinary UTF-8 text containing an occasional NUL as UTF-16.
  const sampleLength = Math.min(buffer.length, 512);
  let evenNuls = 0;
  let oddNuls = 0;
  for (let i = 0; i < sampleLength; i += 1) {
    if (buffer[i] !== 0) continue;
    if (i % 2 === 0) evenNuls += 1;
    else oddNuls += 1;
  }
  const pairs = Math.floor(sampleLength / 2);
  if (pairs >= 4 && oddNuls / pairs > 0.6 && evenNuls / pairs < 0.1) return { encoding: "utf16le", bomBytes: 0 };
  if (pairs >= 4 && evenNuls / pairs > 0.6 && oddNuls / pairs < 0.1) return { encoding: "utf16be", bomBytes: 0 };
  return { encoding: "utf8", bomBytes: 0 };
}

export function decodeTextBuffer(buffer) {
  const { encoding, bomBytes } = detectTextEncoding(buffer);
  const content = buffer.subarray(bomBytes);
  return encoding === "utf16be"
    ? swapUtf16Bytes(content).toString("utf16le")
    : content.toString(encoding);
}

export function createFilesystemTools(cwd, _editTool = null, opts = {}) {
  const fastContext = opts.fastContext !== false && process.env.NOVA_FAST_CONTEXT !== "0";
  const root = resolve(cwd);
  const tools = [];
  if (fastContext) tools.push(
    {
      name: "fast_context",
      description: FAST_CONTEXT_DESCRIPTION,
      parameters: Type.Object({
        keywords: Type.Optional(Type.Array(Type.String(), { minItems: 1, maxItems: 5, description: "1–5 个符号或关键词" })),
        task: Type.Optional(Type.String({ description: "一句话任务描述，用于补充检索词和排序" })),
        files: Type.Optional(Type.Array(Type.String(), { maxItems: 6, description: "已知必看文件，可与 keywords/task 同用" })),
        budget: Type.Optional(Type.Integer({ minimum: 100, maximum: 1200, description: "完整代码单元行预算，默认 600" })),
        maxBytes: Type.Optional(Type.Integer({ minimum: 8192, maximum: 65536, description: "输出硬预算，默认 32768；仅按完整文件/单元边界收敛" })),
        coupling: Type.Optional(Type.Boolean({ description: "开启后附 git 共改耦合提示（近 120 次提交的高频共改文件）" })),
      }),
      async execute(_id, params) {
        const args = params ?? {};
        // fast_context 只有 Rust native 实现（JS 镜像已移除）；无全局 service 时直走 native。
        return textResult(await callContextToolOrLocal("fast_context", root, args, () => callNapiTool("fast_context", root, args)));
      },
    },
    {
      name: "find_symbols",
      description: "并行定位多个符号在仓库中的所有出现位置（文件:行号）。只要行号不要正文时用；需要上下文用 fast_context。",
      parameters: Type.Object({
        names: Type.Array(Type.String(), { minItems: 1, description: "符号名列表" }),
      }),
      async execute(_id, params) {
        const args = params ?? {};
        return textResult(await callContextToolOrLocal("find_symbols", root, args, () => findSymbols(args, root)));
      },
    },
  );
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
  const fastContext = process.env.NOVA_FAST_CONTEXT !== "0";
  const toolLines = [
    "- read: 读取单个文件",
    options.readOnly
      ? "- grep / find / ls: 只读搜索与列举"
      : options.shellConfig?.kind === "powershell"
        ? "- bash: 执行 PowerShell 命令"
        : "- bash: 执行 Bash 命令",
    fastContext ? "- fast_context: 一次打包完整编辑单元 + 依赖定义 + IMPACT/SIG（内部批量 rg + 增量符号索引）" : null,
    fastContext ? "- find_symbols: 并行定位多个符号出现位置（只要行号时用）" : null,
    options.readOnly ? null : "- edit / write: 单文件编辑或写入",
  ].filter(Boolean);

  const stableParts = [
    "你是 Vega：高效、简单、面向软件工程结果。",
    `Available tools:\n${toolLines.join("\n")}`,
      `你拥有 PI coding agent 的原生 read、bash、edit、write 工具。以下工具选择规则是硬性约束。读取内容遵循最小必要原则：已知目标行范围时，只读取相关行段；需要更多上下文时再按需读取相邻行段。需要理解大文件整体结构时改用 fast_context/find_symbols。`
        + (fastContext
          ? "任务涉及跨文件查找或修改（含分析要改哪里）时，先调用一次 fast_context（只要定义/引用行号时用 find_symbols）；一次调用通常替代 5–10 轮 rg+read 往返。拿不准是否涉及多个文件、或只是先分析要改哪里而不写代码时，同样按涉及处理，先调用 fast_context。find_symbols 只用于拿行号；定位后仍需阅读两个及以上文件正文时，把文件清单传给 fast_context 的 files 一次打包，不要逐个 read。已展示范围视为已读，SIG/IMPACT 仅在确需函数体时按 path:line 精确补读；"
          : "未知目标位置时，先用搜索工具定位行号，再读取命中位置附近的必要上下文；")
        + `大文件禁止无目的全量读取。修改已有文件时使用原生 edit；同一文件的多处修改必须合并进同一次 edit 调用的 edits 数组；多个互不依赖的文件可在同轮并行发起多个 edit 调用，但禁止对同一文件并发 edit；后续 edit 的 oldText 若依赖前一个 edit 写出的内容，必须等前者完成后再发起。已知多个独立路径时，同轮并行发多个 read。仅在存在先后依赖或目标重叠时串行调用工具。`,
      (fastContext
        ? "搜索与遍历必须成本有界。路径和行段已明确且只需少量行段时直接 read；任务涉及跨文件查找或修改（含分析要改哪里）时，先调用一次 fast_context（完整 EDIT/DEPS 单元 + IMPACT/SIG；内部批量 rg 与增量符号索引，一次调用通常替代 5–10 轮 rg+read 往返），只要定义/引用位置时用 find_symbols。fast_context 已展示范围视为已读；SIG/IMPACT 仅在确需函数体时精确补读。调用后不要对同一批关键词再用 bash 中的 `rg`/`git grep` 重复发现，也不要仅为查看更多内容放大预算重调；返回 CTX MISS 时按输出中的 next 提示修正符号名或用 files 指定入口文件重试一次，不要退回 rg/grep 逐个搜索。禁止使用 `grep -r` 或 `grep -R` 对仓库根目录或源码根目录进行无排除的递归搜索；兜底搜索默认遵守 `.gitignore`。"
        : "搜索与遍历必须成本有界。禁止使用 `grep -r` 或 `grep -R` 对仓库根目录或源码根目录进行无排除的递归搜索；优先使用 `rg`（遵守 `.gitignore`），仅在需要只搜已跟踪文件时回退 `git grep`。")
        + "除非任务明确要求，不得扫描构建产物、依赖、缓存、生成文件或大型二进制资源目录。`| head`、`| tail` 和输出截断只限制结果展示，不属于工作量限制；递归命令必须通过限定路径、glob、文件类型或排除目录缩小实际扫描范围，并设置较短的 timeout。递归命令超时后不得原样重试，必须缩小范围或改用更合适的搜索工具。",
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

/** Add the configured OpenAI service tier without overriding an explicit payload value. */
export function injectOpenAIServiceTier(payload, serviceTier) {
  if (!payload || typeof payload !== "object" || Array.isArray(payload)) return undefined;
  if (Object.hasOwn(payload, "service_tier")) return undefined;
  if (typeof serviceTier !== "string" || !serviceTier.trim()) return undefined;
  return { ...payload, service_tier: serviceTier.trim() };
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
  const readOperations = {
    access,
    async readFile(path) {
      const buffer = await readFile(path);
      return IMAGE_MEDIA_TYPES[extname(path).toLowerCase()]
        ? buffer
        : Buffer.from(decodeTextBuffer(buffer), "utf8");
    },
    detectImageMimeType(path) {
      return IMAGE_MEDIA_TYPES[extname(path).toLowerCase()];
    },
  };
  const codingTools = options.readOnly
    ? createReadOnlyTools(cwd, { read: { operations: readOperations } })
    : createCodingTools(cwd, { bash: { shellPath: shellConfig.shell }, read: { operations: readOperations } });
  const batchTools = createFilesystemTools(cwd);
  const rawTools = [...batchTools, ...codingTools, ...mcp.tools];
  const archiveDir = options.sessionId
    ? join(alkaidDataRoot(), "tool-results", safeArchiveSegment(options.sessionId))
    : undefined;
  const tools = rawTools.map((tool) => ({
    ...tool,
    async execute(toolCallId, params, signal, onUpdate) {
      const result = await tool.execute(toolCallId, params, signal, onUpdate);
      return governToolResult(result, {
        archiveDir,
        toolCallId,
        toolName: tool.name,
        maxBytes: tool.name === "fast_context" ? FAST_CONTEXT_OUTPUT_MAX_BYTES : undefined,
      });
    },
  }));
  const agentInstructions = await loadAlkaidAgentInstructions(options.agentInstructionsPath);
  const customInstructions = [agentInstructions.trim(), options.systemPrompt?.trim()]
    .filter(Boolean)
    .join("\n\n");
  const systemPrompt = options.systemPromptSnapshot || buildAlkaidSystemPrompt({
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
    prepareNextTurnWithContext: options.prepareNextTurnWithContext
      ? async (context, signal) => {
          const update = await options.prepareNextTurnWithContext(context, signal);
          if (update?.context?.messages) agent.state.messages = update.context.messages;
          return update;
        }
      : undefined,
    onPayload: (payload, model) => {
      const modelApi = model?.api ?? api;
      if (modelApi !== "openai-completions" && modelApi !== "openai-responses") return undefined;
      let next = payload;
      let changed = false;
      const withServiceTier = injectOpenAIServiceTier(next, options.model.serviceTier);
      if (withServiceTier) {
        next = withServiceTier;
        changed = true;
      }
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
  return {
    agent,
    close: () => mcp.close(),
    skills,
    toolCount: tools.length,
    systemPrompt,
    toolShape: tools.map((tool) => ({
      name: tool.name,
      description: tool.description ?? "",
      parameters: tool.parameters ?? tool.inputSchema ?? null,
    })),
  };
}
