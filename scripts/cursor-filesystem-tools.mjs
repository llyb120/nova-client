import { resolve } from "node:path";
import { findSymbols, FAST_CONTEXT_DESCRIPTION } from "./ctx-core.mjs";
import { callNapiTool } from "./nova-napi-tools.mjs";
import { callContextToolOrLocal } from "./nova-context-client.mjs";

/**
 * Vega-style batch FS tools for Cursor SDK `local.customTools`.
 * Exposes code-context tools (fast_context, find_symbols).
 * read_files is intentionally omitted; use Cursor built-in Read.
 * edit_files is intentionally omitted because Cursor
 * CallMcpTool truncates/mangles large or heavily-escaped arguments.
 * Use Cursor built-in Write / StrReplace / Edit for mutations.
 */
export function createCursorFilesystemTools(cwd, options = {}) {
  void options;
  const fastContext = process.env.NOVA_FAST_CONTEXT !== "0";
  const root = resolve(cwd);
  if (!fastContext) return {};
  return {
    fast_context: {
      description: FAST_CONTEXT_DESCRIPTION,
      inputSchema: {
        type: "object",
        properties: {
          keywords: { type: "array", minItems: 1, maxItems: 5, items: { type: "string" }, description: "1–5 个符号或关键词" },
          task: { type: "string", description: "一句话任务描述，用于补充检索词和排序" },
          files: { type: "array", maxItems: 6, items: { type: "string" }, description: "已知必看文件，可与 keywords/task 同用" },
          budget: { type: "integer", minimum: 100, maximum: 1200, description: "完整代码单元行预算，默认 600" },
          maxBytes: { type: "integer", minimum: 8192, maximum: 65536, description: "输出硬预算，默认 32768；仅按完整文�?单元边界收敛" },
          coupling: { type: "boolean", description: "开启后附 git 共改耦合提示（近 120 次提交的高频共改文件）" },
        },
        additionalProperties: false,
      },
      async execute(params) {
        const args = params ?? {};
        // fast_context 只有 Rust native 实现（JS 镜像已移除）；无全局 service 时直走 native。
        return await callContextToolOrLocal("fast_context", root, args, () => callNapiTool("fast_context", root, args));
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
        return await callContextToolOrLocal("find_symbols", root, args, () => findSymbols(args, root));
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
  const fastContext = process.env.NOVA_FAST_CONTEXT !== "0";
  const lines = [
    "You have Cursor built-in Read, Shell, Grep"
      + (fastContext ? " plus Nova tools fast_context and find_symbols" : "")
      + (readOnly ? "" : ", Write/Edit")
      + ". The following tool-selection rules are hard constraints.",
    "Prefer minimal reads: when line ranges are known, read only those segments; expand nearby context only as needed. "
      + (fastContext
        ? "When edit distribution is unknown and you need complete cross-file context, call fast_context once (or find_symbols for definitions/references only). Never re-read shown ranges; read SIG/IMPACT bodies by path:line only when truly needed. "
        : "When location is unknown, search first (see below), then read near hits. ")
      + "Do not dump large files blindly."
      + (readOnly
        ? ""
        : " For edits, use Cursor built-in Write/Edit/StrReplace; do not expect a Nova edit_files tool."),
    (fastContext
      ? "Search and traversal must be cost-bounded. If path and range are known, use Read directly. When edit distribution is unknown, call fast_context once: it returns complete EDIT/DEPS units plus IMPACT/SIG indexes using batched rg and an incremental symbol index; use find_symbols for locations only. Never re-read shown ranges. Read SIG/IMPACT bodies by exact path:line only when truly needed. Do not re-discover the same keywords with Shell rg/git grep or Cursor Grep, and do not re-call merely with a larger budget. Do not use `grep -r` or `grep -R` for unscoped recursive searches of a repo/source root. Fallback searches must honor `.gitignore` by default. "
      : "Search and traversal must be cost-bounded. Do not use `grep -r` or `grep -R` for unscoped recursive searches of a repo/source root. Prefer `rg` (honors `.gitignore`); use `git grep` only as a fallback for tracked-only searches. ")
      + "Unless the task requires it, do not scan build artifacts, dependencies, caches, generated files, or large binary asset dirs. `| head` / `| tail` and output truncation only limit display, not work; recursive commands must narrow via path/glob/type/excludes and use a short timeout. After a recursive timeout, do not retry the same command unchanged—narrow scope or switch tools.",
  ];
  if (readOnly) {
    lines.push("Current mode is plan/read-only: analyze only; do not modify files.");
  }
  return lines.join("\n");
}

/** Every-turn prompt prefix: batch FS/search policy. */
export function cursorPromptPrefix(options = {}) {
  return cursorBatchToolPolicy(options);
}
