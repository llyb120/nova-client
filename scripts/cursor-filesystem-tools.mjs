import { createReadStream } from "node:fs";
import { resolve } from "node:path";
import { createInterface } from "node:readline";
import { contextBundle, findSymbols, FAST_CONTEXT_DESCRIPTION } from "./ctx-core.mjs";
import { callNativeToolOrFallback } from "./nova-native-tools.mjs";

const DEFAULT_BATCH_READ_LINES = 2000;
/** Match Vega / pi coding tools: keep read_files outputs usable without blowing the context window. */
const READ_FILES_MAX_BYTES = 32 * 1024;

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

function resolveInputPath(root, input) {
  return resolve(root, input);
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

const PATH_OR_RANGE_SCHEMA = {
  anyOf: [
    { type: "string" },
    {
      type: "object",
      properties: {
        path: { type: "string" },
        offset: { type: "integer", minimum: 1 },
        limit: { type: "integer", minimum: 1, maximum: 2000 },
      },
      required: ["path"],
      additionalProperties: false,
    },
  ],
};

/**
 * Vega-style batch FS tools for Cursor SDK `local.customTools`.
 * Exposes read_files plus code-context tools (fast_context, find_symbols).
 * edit_files is intentionally omitted because Cursor
 * CallMcpTool truncates/mangles large or heavily-escaped arguments.
 * Use Cursor built-in Write / StrReplace / Edit for mutations.
 */
export function createCursorFilesystemTools(cwd, options = {}) {
  void options;
  const fastContext = process.env.NOVA_FAST_CONTEXT !== "0";
  const root = resolve(cwd);
  return {
    read_files: {
      description: `同一读取阶段已有两个及以上路径已知、互不依赖的 UTF-8 文本目标时必须调用一次本工具，不得拆成多个 Read；内部并行、流式读取，默认每个文件读取前 ${DEFAULT_BATCH_READ_LINES} 行（且不超过约 32KB）。请为每个文件按需指定 offset/limit，并用返回的 nextOffset 继续读取。`,
      inputSchema: {
        type: "object",
        properties: {
          paths: {
            type: "array",
            minItems: 1,
            items: PATH_OR_RANGE_SCHEMA,
          },
        },
        required: ["paths"],
        additionalProperties: false,
      },
      async execute({ paths }) {
        const list = Array.isArray(paths) ? paths : [];
        const results = await callNativeToolOrFallback("read_files", root, { paths: list }, async () =>
          Promise.all(list.map(async (input) => {
            const request = typeof input === "string" ? { path: input } : (input ?? {});
            const requestPath = String(request.path ?? "");
            try {
              const path = resolveInputPath(root, requestPath);
              const result = await readTextLines(path, request.offset, request.limit);
              return {
                path: requestPath,
                content: result.content,
                ...(result.truncated ? { truncated: true, nextOffset: result.nextOffset } : {}),
              };
            } catch (error) {
              return { path: requestPath, error: error instanceof Error ? error.message : String(error) };
            }
          })));
        return JSON.stringify(results);
      },
    },
    ...(fastContext ? {
    fast_context: {
      description: FAST_CONTEXT_DESCRIPTION,
      inputSchema: {
        type: "object",
        properties: {
          keywords: { type: "array", minItems: 1, maxItems: 5, items: { type: "string" }, description: "1–5 个符号或关键词" },
          task: { type: "string", description: "一句话任务描述，用于补充检索词和排序" },
          files: { type: "array", maxItems: 6, items: { type: "string" }, description: "已知必看文件，可与 keywords/task 同用" },
          budget: { type: "integer", minimum: 100, maximum: 1200, description: "完整代码单元行预算，默认 600" },
          maxBytes: { type: "integer", minimum: 8192, maximum: 65536, description: "输出硬预算，默认 32768；仅按完整文件/单元边界收敛" },
        },
        additionalProperties: false,
      },
      async execute(params) {
        const args = params ?? {};
        return await callNativeToolOrFallback("fast_context", root, args, () => contextBundle(args, root));
      },
    },
    find_symbols: {
      description: "并行定位多个符号在仓库中的所有出现位置（文件:行号）。只要行号不要正文时用；需要上下文用 fast_context。",
      inputSchema: {
        type: "object",
        properties: { names: { type: "array", minItems: 1, items: { type: "string" }, description: "符号名列表" } },
        required: ["names"],
        additionalProperties: false,
      },
      async execute(params) {
        const args = params ?? {};
        return await callNativeToolOrFallback("find_symbols", root, args, () => findSymbols(args, root));
      },
    },
    } : {}),
  };
}

/**
 * Hard tool-selection / search policy. Cursor has no durable systemPrompt field, and Nova
 * creates a fresh Agent per user turn, so this prefix must be attached on every prompt.
 */
export function cursorBatchToolPolicy(options = {}) {
  const readOnly = options.readOnly === true;
  const fastContext = process.env.NOVA_FAST_CONTEXT !== "0";
  const lines = [
    "You have Nova batch tool read_files plus Cursor built-in Read, Shell, Grep"
      + (readOnly ? "" : ", Write/Edit")
      + ". The following tool-selection rules are hard constraints.",
      "Before each read phase, inventory known targets: if there is only one target, use Read; when two or more independent UTF-8 text paths are already known in the same read phase, you must merge them into one read_files call and set per-file offset/limit as needed. Do not call Read repeatedly, and do not use parallel wrappers of multiple Read calls instead of read_files. Wanting to understand files in order is not a read dependency. Use Read only when a later path/range depends on a prior result, the target is not UTF-8 text, or only one file is needed. When later independent text targets appear, the next read phase must again use read_files. Prefer minimal reads: when line ranges are known, read only those segments; expand nearby context only as needed. "
        + (fastContext
          ? "When edit distribution is unknown and you need complete cross-file context, call fast_context once (or find_symbols for definitions/references only). Never re-read shown ranges; read SIG/IMPACT bodies by path:line only when truly needed. "
          : "When location is unknown, search first (see below), then read near hits. ")
        + "Do not dump large files blindly."
        + (readOnly
          ? ""
          : " For edits, use Cursor built-in Write/Edit/StrReplace; do not expect a Nova edit_files tool."),
      (fastContext
        ? "Search and traversal must be cost-bounded. If path and range are known, use Read/read_files directly. When edit distribution is unknown, call fast_context once: it returns complete EDIT/DEPS units plus IMPACT/SIG indexes using batched rg and an incremental symbol index; use find_symbols for locations only. Never re-read shown ranges. Read SIG/IMPACT bodies by exact path:line only when truly needed. Do not re-discover the same keywords with Shell rg/git grep or Cursor Grep, and do not re-call merely with a larger budget. Do not use `grep -r` or `grep -R` for unscoped recursive searches of a repo/source root. Fallback searches must honor `.gitignore` by default. "
        : "Search and traversal must be cost-bounded. Do not use `grep -r` or `grep -R` for unscoped recursive searches of a repo/source root. Prefer `rg` (honors `.gitignore`); use `git grep` only as a fallback for tracked-only searches. ")
        + "Unless the task requires it, do not scan build artifacts, dependencies, caches, generated files, or large binary asset dirs. `| head` / `| tail` and output truncation only limit display, not work; recursive commands must narrow via path/glob/type/excludes and use a short timeout. After a recursive timeout, do not retry the same command unchanged—narrow scope or switch tools.",
  ];
  if (readOnly) {
    lines.push("Current mode is plan/read-only: analyze only; do not modify files.");
  }
  return lines.join("\n");
}

/**
 * Vega-style caveman full reply policy (same text as Alkaid/Vega system prompt).
 * Attached every turn: no durable systemPrompt; fresh Agent each send.
 * Intensity matches JuliusBrussee/caveman SKILL.md `full` (default).
 */
export function cursorCavemanPolicy() {
  return "Respond terse like smart caveman. All technical substance stay. Only fluff die. 默认 full：去冠词、套话、寒暄、模糊措辞；断句可；短同义词。技术实质全留。先给结论，再给行动所需信息。无工具旁白，无装饰表格/emoji，长日志只引关键行。代码、命令、API、错误原文须准确。跟随用户主语言压缩文风，不强制英文。安全警告、不可逆确认、多步顺序易歧义、压缩造成技术歧义、用户要求澄清时恢复完整句；其余保持 caveman。按用户要求增减细节。";
}

/** Combined every-turn prompt prefix: batch FS/search policy + caveman style. */
export function cursorPromptPrefix(options = {}) {
  return [cursorBatchToolPolicy(options), cursorCavemanPolicy()].filter(Boolean).join("\n\n");
}
