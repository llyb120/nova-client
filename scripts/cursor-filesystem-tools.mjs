import { createReadStream } from "node:fs";
import { readFile, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { applySmartEdits } from "./alkaid-smart-edit.mjs";
import { searchSessionHistory } from "./session-history-search.mjs";

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

function textEditFromLines(edit, fileIndex, editIndex) {
  const oldLines = edit?.oldLines;
  const newLines = edit?.newLines;
  const valid = Array.isArray(oldLines) && oldLines.length > 0
    && Array.isArray(newLines) && newLines.length > 0
    && oldLines.every((line) => typeof line === "string")
    && newLines.every((line) => typeof line === "string");
  if (!valid) {
    throw new Error(`files[${fileIndex}].edits[${editIndex}] requires non-empty string arrays oldLines/newLines`);
  }
  return { oldText: oldLines.join("\n"), newText: newLines.join("\n") };
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
 * Plan / read-only mode omits edit_files.
 */
export function createCursorFilesystemTools(cwd, options = {}) {
  const root = resolve(cwd);
  const readOnly = options.readOnly === true;
  const tools = {
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
  };

  const novaRoot = process.env.NOVA_DATA_DIR || join(homedir(), ".nova");
  tools.search_session_history = {
    description: "按需检索 Vega/Cursor 本地历史会话。仅在当前上下文缺少旧决策或用户明确要求查找历史时使用；返回 BM25 风格排序的少量片段。",
    inputSchema: {
      type: "object",
      properties: {
        query: { type: "string", minLength: 1 },
        limit: { type: "integer", minimum: 1, maximum: 10 },
      },
      required: ["query"],
      additionalProperties: false,
    },
    async execute({ query, limit }) {
      return JSON.stringify(await searchSessionHistory([
        join(novaRoot, "alkaid", "sessions"),
        join(novaRoot, "cursor-slim-memory"),
      ], query, { limit }));
    },
  };

  if (!readOnly) {
    tools.edit_files = {
      description: "并行智能编辑多个互不依赖的文件。每段 oldLines/newLines 必须按行传为 JSON 字符串数组（不要把多行内容塞进一个字符串），以避开 Cursor 对多行工具参数的解析缺陷。先精确匹配，再智能定位；歧义或重叠时拒绝，所有文件验证成功后才并行写入。",
      inputSchema: {
        type: "object",
        properties: {
          files: {
            type: "array",
            minItems: 1,
            items: {
              type: "object",
              properties: {
                path: { type: "string" },
                edits: {
                  type: "array",
                  minItems: 1,
                  items: {
                    type: "object",
                    properties: {
                      oldLines: {
                        type: "array",
                        minItems: 1,
                        items: { type: "string" },
                        description: "待替换文本，每个数组元素严格对应一行；用末尾空字符串表示结尾换行。",
                      },
                      newLines: {
                        type: "array",
                        minItems: 1,
                        items: { type: "string" },
                        description: "替换后文本，每个数组元素严格对应一行；用末尾空字符串表示结尾换行。",
                      },
                    },
                    required: ["oldLines", "newLines"],
                    additionalProperties: false,
                  },
                },
              },
              required: ["path", "edits"],
              additionalProperties: false,
            },
          },
        },
        required: ["files"],
        additionalProperties: false,
      },
      async execute({ files }) {
        const list = Array.isArray(files) ? files : [];
        if (list.length === 0) throw new Error("edit_files requires a non-empty files array");
        const grouped = new Map();
        for (const [fileIndex, file] of list.entries()) {
          const requestPath = String(file?.path ?? "");
          const inputEdits = Array.isArray(file?.edits) ? file.edits : [];
          if (!requestPath || inputEdits.length === 0) {
            throw new Error(`files[${fileIndex}] requires path and a non-empty edits array`);
          }
          const edits = inputEdits.map((edit, editIndex) => textEditFromLines(edit, fileIndex, editIndex));
          const target = resolveInputPath(root, requestPath);
          const existing = grouped.get(target);
          if (existing) existing.edits.push(...edits);
          else grouped.set(target, { path: requestPath, target, edits });
        }
        const targets = [...grouped.values()];

        // Read and locate every edit against immutable snapshots before writing any file.
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

        const writes = await Promise.allSettled(prepared.map((file) => writeFile(file.target, file.output, "utf8")));
        const failed = writes.findIndex((result) => result.status === "rejected");
        if (failed >= 0) {
          await Promise.allSettled(prepared.map((file, index) =>
            writes[index].status === "fulfilled" ? writeFile(file.target, file.original, "utf8") : Promise.resolve()));
          const reason = writes[failed].reason;
          throw reason instanceof Error ? reason : new Error(String(reason));
        }
        // Return a JSON string (same as read_files). Object returns with nested
        // undefined break Cursor SDK/MCP protobuf Value decoding even after a
        // successful write, which made edit_files look "always failed".
        return JSON.stringify({
          message: `已并行智能编辑 ${prepared.length} 个文件`,
          paths: prepared.map((file) => file.path),
          matches: prepared.map((file) => ({ path: file.path, edits: file.matches })),
        });
      },
    };
  }

  return tools;
}

/**
 * Hard tool-selection / search policy. Cursor has no durable systemPrompt field, and Nova
 * creates a fresh Agent per user turn, so this prefix must be attached on every prompt.
 */
export function cursorBatchToolPolicy(options = {}) {
  const readOnly = options.readOnly === true;
  const lines = [
    "You have Nova batch tools read_files / search_session_history"
      + (readOnly ? "" : " / edit_files")
      + " plus Cursor built-in Read, Shell, Grep"
      + (readOnly ? "" : ", Write/Edit")
      + ". The following tool-selection rules are hard constraints.",
    "Before each read phase, inventory known targets: if there is only one target, use Read; when two or more independent UTF-8 text paths are already known in the same read phase, you must merge them into one read_files call and set per-file offset/limit as needed. Do not call Read repeatedly, and do not use parallel wrappers of multiple Read calls instead of read_files. Wanting to understand files in order is not a read dependency. Use Read only when a later path/range depends on a prior result, the target is not UTF-8 text, or only one file is needed. When later independent text targets appear, the next read phase must again use read_files. Prefer minimal reads: when line ranges are known, read only those segments; expand nearby context only as needed. When location is unknown, search first (see below), then read near hits. Do not dump large files blindly."
      + (readOnly
        ? ""
        : " When editing two or more independent existing files, you must use edit_files; merge multiple edits for the same file into one files[] entry. Serialize tool calls only when there is a real dependency or overlapping targets."),
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
