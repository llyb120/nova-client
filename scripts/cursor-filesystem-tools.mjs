import { createReadStream } from "node:fs";
import { resolve } from "node:path";
import { createInterface } from "node:readline";
import { contextBundle, findSymbol } from "./ctx-core.mjs";

const DEFAULT_BATCH_READ_LINES = 200;
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
 * Exposes read_files plus code-context tools (context_bundle, find_symbol).
 * edit_files is intentionally omitted because Cursor
 * CallMcpTool truncates/mangles large or heavily-escaped arguments.
 * Use Cursor built-in Write / StrReplace / Edit for mutations.
 */
export function createCursorFilesystemTools(cwd, options = {}) {
  void options;
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
        const results = await Promise.all(list.map(async (input) => {
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
        }));
        return JSON.stringify(results);
      },
    },
    context_bundle: {
      description: "按关键词/符号一次性打包相关代码上下文：命中文件按相关度分层装配（小文件全文、大文件给符号大纲+命中段），并附 1 跳调用邻居大纲。用于在分析或修改前快速获取代码全貌，避免逐文件试探式读取。基于 git grep + rg，无需预建索引。",
      inputSchema: {
        type: "object",
        properties: {
          keywords: { type: "array", minItems: 1, items: { type: "string" }, description: "关键词或符号名列表" },
          budget: { type: "integer", minimum: 100, maximum: 4000, description: "总行数预算，默认 700" },
          ctx: { type: "integer", minimum: 0, maximum: 60, description: "命中行上下文半径，默认 12" },
          maxFiles: { type: "integer", minimum: 1, maximum: 40, description: "核心命中文件上限，默认 12" },
        },
        required: ["keywords"],
        additionalProperties: false,
      },
      async execute(params) {
        return contextBundle(params ?? {}, root);
      },
    },
    find_symbol: {
      description: "快速定位符号在仓库中的所有出现位置（文件:行号），基于 git grep。用于在读取/修改前确认符号分布。",
      inputSchema: {
        type: "object",
        properties: { name: { type: "string", description: "符号名" } },
        required: ["name"],
        additionalProperties: false,
      },
      async execute(params) {
        return findSymbol(params ?? {}, root);
      },
    },
  };
}

/**
 * Hard tool-selection / search policy. Cursor has no durable systemPrompt field, and Nova
 * creates a fresh Agent per user turn, so this prefix must be attached on every prompt.
 */
export function cursorBatchToolPolicy(options = {}) {
  const readOnly = options.readOnly === true;
  const lines = [
    "You have Nova batch tool read_files plus Cursor built-in Read, Shell, Grep"
      + (readOnly ? "" : ", Write/Edit")
      + ". The following tool-selection rules are hard constraints.",
    "Before each read phase, inventory known targets: if there is only one target, use Read; when two or more independent UTF-8 text paths are already known in the same read phase, you must merge them into one read_files call and set per-file offset/limit as needed. Do not call Read repeatedly, and do not use parallel wrappers of multiple Read calls instead of read_files. Wanting to understand files in order is not a read dependency. Use Read only when a later path/range depends on a prior result, the target is not UTF-8 text, or only one file is needed. When later independent text targets appear, the next read phase must again use read_files. Prefer minimal reads: when line ranges are known, read only those segments; expand nearby context only as needed. When location is unknown, search first (see below), then read near hits. Do not dump large files blindly."
      + (readOnly
        ? ""
        : " For edits, use Cursor built-in Write/Edit/StrReplace; do not expect a Nova edit_files tool."),
    "Search and traversal must be cost-bounded. Do not use `grep -r` or `grep -R` for unscoped recursive searches of a repo/source root. Prefer `git grep` for tracked files; use `rg` when untracked files matter, and honor `.gitignore` by default. Cursor Grep is also allowed. Unless the task requires it, do not scan build artifacts, dependencies, caches, generated files, or large binary asset dirs. `| head` / `| tail` and output truncation only limit display, not work; recursive commands must narrow via path/glob/type/excludes and use a short timeout. After a recursive timeout, do not retry the same command unchanged—narrow scope or switch tools.",
  ];
  if (readOnly) {
    lines.push("Current mode is plan/read-only: analyze only; do not modify files.");
  }
  return lines.join("\n");
}

/**
 * Vega-style concise reply policy (same text as Alkaid/Vega system prompt).
 * Attached every turn: no durable systemPrompt; fresh Agent each send.
 */
export function cursorCavemanPolicy() {
  return "回复默认简洁专业，使用完整句子并保留必要解释。先给结论，再给行动所需信息。省略寒暄、套话、复述、工具旁白和重复总结。简单问题简答，复杂问题按需展开。不用装饰性表格或长日志，只引关键错误。代码、命令、API 和错误原文须准确；安全警告、不可逆操作确认和必要步骤不得省略。按用户要求增减细节。";
}

/** Combined every-turn prompt prefix: batch FS/search policy + caveman style. */
export function cursorPromptPrefix(options = {}) {
  return [cursorBatchToolPolicy(options), cursorCavemanPolicy()].filter(Boolean).join("\n\n");
}
