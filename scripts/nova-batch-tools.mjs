import { createReadStream } from "node:fs";
import { readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { createInterface } from "node:readline";
import { applySmartEdits } from "./alkaid-smart-edit.mjs";
import { contextBundle, findSymbols, FAST_CONTEXT_DESCRIPTION } from "./ctx-core.mjs";
import { callNapiToolOrFallback } from "./nova-napi-tools.mjs";

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

function fastContextEnabled(options = {}) {
  if (typeof options.fastContext === "boolean") return options.fastContext;
  return process.env.NOVA_FAST_CONTEXT !== "0";
}

function readOnlyEnabled(options = {}) {
  if (typeof options.readOnly === "boolean") return options.readOnly;
  return process.env.NOVA_TOOLS_READ_ONLY === "1";
}

function stringList(value) {
  const list = Array.isArray(value) ? value : typeof value === "string" ? [value] : [];
  return [...new Set(list.map((item) => String(item ?? "").trim()).filter(Boolean))];
}

export function normalizeFastContextArgs(params = {}) {
  const query = String(params.query ?? "").trim();
  const keywords = stringList(params.keywords);
  const task = String(params.task ?? "").trim() || (query.includes(" ") ? query : "");
  if (!keywords.length && query && !task) keywords.push(query);
  const files = stringList(params.files);
  return {
    ...params,
    keywords,
    task,
    files,
  };
}

export function normalizeFindSymbolsArgs(params = {}) {
  const names = stringList(params.names);
  if (!names.length) names.push(...stringList(params.symbols));
  if (!names.length) names.push(...stringList(params.keywords));
  if (!names.length) names.push(...stringList(params.name));
  if (!names.length) names.push(...stringList(params.query));
  return { names: names.slice(0, 12) };
}

/**
 * Shared Nova batch FS / context tools for Cursor customTools and Devin ACP MCP.
 * @param {string} cwd
 * @param {{ readOnly?: boolean, fastContext?: boolean, includeEditFiles?: boolean }} [options]
 */
export function createNovaBatchTools(cwd, options = {}) {
  const fastContext = fastContextEnabled(options);
  const readOnly = readOnlyEnabled(options);
  const includeEditFiles = options.includeEditFiles !== false && !readOnly;
  const root = resolve(cwd);

  /** @type {Record<string, { description: string, inputSchema: object, execute: (args: any) => Promise<string> }>} */
  const tools = {
    read_files: {
      description: `远程 MCP 端点，只能通过 Devin 顶层 mcp_call_tool 调用：server_name="nova-tools", tool_name="read_files"；禁止把 read_files 当作顶层工具直接调用。同一读取阶段已有两个及以上路径已知、互不依赖的 UTF-8 文本目标时合并调用；默认每个文件前 ${DEFAULT_BATCH_READ_LINES} 行（且不超过约 32KB）。首选参数 paths，也兼容 files；请按需设置 offset/limit。未知精确目标范围时省略 limit，使用默认 2000 行；禁止随意选择 100/200 行小分页。仅已知精确目标范围时使用较小 limit。必要的 nextOffset 续读也应省略 limit 或使用 2000。nextOffset 仅供按需续读；truncated 只表示仍有后续内容。不要为了消除 truncated 顺序读完整个文件，仅当当前任务缺少必要上下文时继续；需要理解大文件整体结构时改用 fast_context/find_symbols。`,
      inputSchema: {
        type: "object",
        properties: {
          paths: {
            type: "array",
            minItems: 1,
            items: PATH_OR_RANGE_SCHEMA,
            description: "首选参数：文件路径或带 offset/limit 的读取范围",
          },
          files: {
            type: "array",
            minItems: 1,
            items: PATH_OR_RANGE_SCHEMA,
            description: "兼容别名；语义与 paths 完全相同",
          },
        },
        anyOf: [{ required: ["paths"] }, { required: ["files"] }],
        additionalProperties: false,
      },
      async execute(params = {}) {
        const requested = Array.isArray(params.paths) ? params.paths : params.files;
        const list = Array.isArray(requested) ? requested : [];
        if (!list.length) throw new Error("read_files 需要非空 paths（也兼容 files）");
        const results = await callNapiToolOrFallback("read_files", root, { paths: list }, async () =>
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
  };

  if (fastContext) {
    tools.fast_context = {
      description: `远程 MCP 端点，只能通过 Devin 顶层 mcp_call_tool 调用：server_name="nova-tools", tool_name="fast_context"；禁止把 fast_context 当作顶层工具直接调用。${FAST_CONTEXT_DESCRIPTION} 必须提供 keywords、query、task 或 files 至少一个；例如 {"query":"cursor bridge"}。`,
      inputSchema: {
        type: "object",
        properties: {
          keywords: { type: "array", minItems: 1, items: { type: "string", minLength: 1 }, description: "首选参数：关键词或符号名，建议 2–6 个" },
          query: { type: "string", minLength: 1, description: "简短检索词；兼容单字符串调用，如 cursor" },
          task: { type: "string", minLength: 1, description: "自然语言任务描述，将自动提取检索词" },
          files: { type: "array", minItems: 1, items: { type: "string", minLength: 1 }, description: "明确要纳入上下文的仓库相对文件路径" },
          maxChars: { type: "integer", minimum: 4000, maximum: 80000, description: "输出字符预算，通常无需设置" },
          budget: { type: "integer", minimum: 100, maximum: 4000, description: "兼容旧参数：行预算，通常无需设置" },
        },
        anyOf: [
          { required: ["keywords"] },
          { required: ["query"] },
          { required: ["task"] },
          { required: ["files"] },
        ],
        additionalProperties: false,
      },
      async execute(params) {
        const args = normalizeFastContextArgs(params);
        return await contextBundle(args, root);
      },
    };
    tools.find_symbols = {
      description: "远程 MCP 端点，只能通过 Devin 顶层 mcp_call_tool 调用：server_name=\"nova-tools\", tool_name=\"find_symbols\"；禁止把 find_symbols 当作顶层工具直接调用。用于定位明确符号名的定义和引用；必须提供 names（推荐），也兼容 name/query/keywords。需要理解上下文时通过同一路由调用 fast_context。",
      inputSchema: {
        type: "object",
        properties: {
          names: { type: "array", minItems: 1, items: { type: "string", minLength: 1 }, description: "首选参数：明确符号名列表" },
          name: { type: "string", minLength: 1, description: "单个符号名" },
          query: { type: "string", minLength: 1, description: "单个符号名的兼容参数" },
          keywords: { type: "array", minItems: 1, items: { type: "string", minLength: 1 }, description: "兼容符号名数组" },
          symbols: { type: "array", minItems: 1, items: { type: "string", minLength: 1 }, description: "兼容符号名数组" },
        },
        anyOf: [
          { required: ["names"] },
          { required: ["name"] },
          { required: ["query"] },
          { required: ["keywords"] },
          { required: ["symbols"] },
        ],
        additionalProperties: false,
      },
      async execute(params) {
        const args = normalizeFindSymbolsArgs(params);
        return await findSymbols(args, root);
      },
    };
  }

  if (includeEditFiles) {
    tools.edit_files = {
      description: "远程 MCP 端点，只能通过 Devin 顶层 mcp_call_tool 调用：server_name=\"nova-tools\", tool_name=\"edit_files\"；禁止把 edit_files 当作顶层工具直接调用。并行智能编辑多个互不依赖的文件；歧义或重叠时拒绝，全部验证成功后才写入。",
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
                      oldText: { type: "string" },
                      newText: { type: "string" },
                    },
                    required: ["oldText", "newText"],
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
        return JSON.stringify(await callNapiToolOrFallback("edit_files", root, { files: list }, async () => {
          const grouped = new Map();
        for (const file of list) {
          const requestPath = String(file?.path ?? "");
          const target = resolveInputPath(root, requestPath);
          const edits = Array.isArray(file?.edits) ? file.edits : [];
          const existing = grouped.get(target);
          if (existing) existing.edits.push(...edits);
          else grouped.set(target, { path: requestPath, target, edits: [...edits] });
        }
        const targets = [...grouped.values()];
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
          return {
            message: `已并行智能编辑 ${prepared.length} 个文件`,
            paths: prepared.map((file) => file.path),
            matches: prepared.map((file) => ({ path: file.path, edits: file.matches })),
          };
        }));
      },
    };
  }

  return tools;
}

/**
 * Hard tool-selection policy for Devin (ACP MCP). Injected on first prompt of a new session.
 */
export function novaDevinBatchToolPolicy(options = {}) {
  const readOnly = readOnlyEnabled(options);
  const fastContext = fastContextEnabled(options);
  const includeEditFiles = options.includeEditFiles !== false && !readOnly;
  const lines = [
    "ROUTING RULE — before choosing any tool: Nova endpoints must NEVER be selected as direct tool calls. Select Devin's top-level mcp_call_tool first, then pass server_name=\"nova-tools\" and the endpoint name in tool_name. You have Nova MCP endpoints from server nova-tools (read_files"
      + (fastContext ? ", fast_context, find_symbols" : "")
      + (includeEditFiles ? ", edit_files" : "")
      + ") plus Devin built-in tools. In this Devin version, read_files, fast_context, find_symbols, and edit_files are remote MCP tool names, NOT top-level callable Devin tools. Never select or invoke any of those names directly, even after mcp_list_tools lists them; a direct invocation produces `Unknown tool ... This tool is not available.` Your only valid execution path for a Nova tool is Devin's generic mcp_call_tool wrapper. Set server_name to the top-level string \"nova-tools\" (never omit it or put it inside arguments), and put only the selected Nova tool's inputs in arguments. Example: {\"server_name\":\"nova-tools\",\"tool_name\":\"fast_context\",\"arguments\":{\"query\":\"cursor\"}}. Follow the wrapper's declared tool-name field if its schema uses a different spelling. The available Nova tools are already stated above; do not call mcp_list_tools merely to discover them. In every rule below, wording such as `use/call read_files` means `call mcp_call_tool with server_name nova-tools and tool_name read_files`; it never authorizes a direct tool call. If a direct call reports `Unknown tool`, retry once through mcp_call_tool. If parsing reports missing field `server_name`, correct the wrapper call once. Never repeat a malformed call unchanged. The following tool-selection rules are hard constraints.",
    "Before each read phase, inventory known targets: if there is only one target, use Devin's native read; when two or more independent UTF-8 text paths are already known in the same read phase, you must merge them into one read_files call and set per-file offset/limit as needed. Do not call native read repeatedly, and do not use parallel wrappers of multiple native reads instead of read_files. Wanting to understand files in order is not a read dependency. Use native read only when a later path/range depends on a prior result, the target is not UTF-8 text, or only one file is needed. When later independent text targets appear, the next read phase must again use read_files. Prefer minimal reads: when line ranges are known, read only those segments; expand nearby context only as needed. When the exact target range is unknown, omit limit so read_files uses its 2000-line default; never invent arbitrary 100/200-line pages. Use a smaller limit only for a known exact range. A necessary nextOffset continuation must also omit limit or use 2000. A returned nextOffset is only for on-demand continuation; truncated only means more content exists. Never sequentially page through a whole file merely to clear truncated. Continue only when the current task still lacks required context; use fast_context/find_symbols to understand a large file's structure. "
      + (fastContext
        ? "When location is unknown, you must call only fast_context (or find_symbols if you only need line numbers); then read only coverage gaps / next_reads. "
        : "When location is unknown, search first (see below), then read near hits. ")
      + "Do not dump large files blindly."
      + (includeEditFiles
        ? " When modifying two or more independent existing files, you must use edit_files; merge multiple edits for the same file into that file's edits array. Single-file edits may use Devin native edit tools."
        : " For edits, use Devin native edit tools; do not expect a Nova edit_files tool in this mode."),
    (fastContext
      ? "Search and traversal must be cost-bounded. When symbol/keyword distribution or surrounding code is unknown, you MUST call only fast_context (packs definition bodies + 1-hop neighbors + coverage; internal rg, honors `.gitignore`) or find_symbols (locations only). Do not re-read FULL/BODY.covered ranges; fill gaps via next_reads with one read_files. After fast_context/find_symbols, do not re-discover the same keywords with shell `rg`/`git grep` or Devin grep—rg is already inside fast_context. External rg/grep/git grep are allowed only when: (1) next_reads/gaps are still insufficient, or (2) the task explicitly needs a scoped literal search that fast_context did not cover. Do not use `grep -r` or `grep -R` for unscoped recursive searches of a repo/source root. Fallback searches must honor `.gitignore` by default. "
      : "Search and traversal must be cost-bounded. Do not use `grep -r` or `grep -R` for unscoped recursive searches of a repo/source root. Prefer `rg` (honors `.gitignore`); use `git grep` only as a fallback for tracked-only searches. ")
      + "Unless the task requires it, do not scan build artifacts, dependencies, caches, generated files, or large binary asset dirs. `| head` / `| tail` and output truncation only limit display, not work; recursive commands must narrow via path/glob/type/excludes and use a short timeout. After a recursive timeout, do not retry the same command unchanged—narrow scope or switch tools.",
  ];
  if (readOnly) {
    lines.push("Current mode is plan/read-only: analyze only; do not modify files.");
  }
  return lines.join("\n");
}
