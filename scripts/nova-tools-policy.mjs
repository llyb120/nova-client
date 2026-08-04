import { existsSync } from "node:fs";

/**
 * Vega Reasonix tool layer for external SDK backends (CodeBuddy / OpenCode).
 *
 * The host materializes `nova-tools-mcp.mjs` and passes its path plus the
 * fast-context switch through the bridge environment:
 *   NOVA_TOOLS_MCP_SCRIPT              — bundled nova-tools MCP stdio server
 *   NOVA_FAST_CONTEXT                  — "0" disables fast_context / find_symbols
 *   NOVA_CONTEXT_SERVICE_ENDPOINT/TOKEN — inherited into the MCP server env
 *
 * Backends that own their session transcript (CodeBuddy CLI, opencode server)
 * receive the tool half of Reasonix here; the slim-memory half stays with the
 * Vega/Cursor bridges, which control the native message list.
 */

export function novaToolsScript(fileExists = existsSync) {
  const script = process.env.NOVA_TOOLS_MCP_SCRIPT;
  if (!script || !fileExists(script)) return undefined;
  return script;
}

export function novaFastContextEnabled() {
  return process.env.NOVA_FAST_CONTEXT !== "0";
}

/** Any Nova tool exposed in this mode? Otherwise skip attaching the MCP server. */
export function novaToolsAttached(request, { fileExists = existsSync } = {}) {
  if (!novaToolsScript(fileExists)) return false;
  const readOnly = request?.mode === "plan";
  return novaFastContextEnabled() || !readOnly;
}

export function novaToolsMcpEnv(cwd, { fastContext, readOnly }) {
  const env = {
    NOVA_TOOLS_CWD: cwd,
    NOVA_FAST_CONTEXT: fastContext ? "1" : "0",
  };
  if (readOnly) env.NOVA_TOOLS_READ_ONLY = "1";
  if (process.env.NOVA_CONTEXT_SERVICE_ENDPOINT) {
    env.NOVA_CONTEXT_SERVICE_ENDPOINT = process.env.NOVA_CONTEXT_SERVICE_ENDPOINT;
  }
  if (process.env.NOVA_CONTEXT_SERVICE_TOKEN) {
    env.NOVA_CONTEXT_SERVICE_TOKEN = process.env.NOVA_CONTEXT_SERVICE_TOKEN;
  }
  return env;
}

function novaToolsShape(request, fileExists) {
  if (!novaToolsAttached(request, { fileExists })) return undefined;
  const readOnly = request?.mode === "plan";
  return {
    script: novaToolsScript(fileExists),
    cwd: request?.cwd || process.cwd(),
    fastContext: novaFastContextEnabled(),
    readOnly,
  };
}

/** CodeBuddy SDK `mcpServers` + appended system prompt for one prompt request. */
export function codebuddyNovaTools(request, { fileExists = existsSync } = {}) {
  const shape = novaToolsShape(request, fileExists);
  if (!shape) return {};
  return {
    mcpServers: {
      "nova-tools": {
        type: "stdio",
        command: process.execPath,
        args: [shape.script],
        env: novaToolsMcpEnv(shape.cwd, shape),
      },
    },
    systemPrompt: { append: novaToolsPolicy(shape) },
  };
}

/** opencode `Config` fragment (mcp section) for the per-turn server. */
export function opencodeNovaTools(request, { fileExists = existsSync } = {}) {
  const shape = novaToolsShape(request, fileExists);
  if (!shape) return {};
  return {
    mcp: {
      "nova-tools": {
        type: "local",
        command: [process.execPath, shape.script],
        environment: novaToolsMcpEnv(shape.cwd, shape),
        enabled: true,
      },
    },
  };
}

/**
 * Tool-selection policy. Mirrors the Vega/Cursor batch FS/search rules, but
 * references the Nova tools as nova-tools MCP endpoints because external
 * backends expose them under server-prefixed names (exact spelling varies by
 * host and is visible in the model's tool list).
 */
export function novaToolsPolicy({ fastContext = novaFastContextEnabled(), readOnly = false } = {}) {
  const toolNames = [];
  if (fastContext) toolNames.push("fast_context", "find_symbols");
  if (!readOnly) toolNames.push("edit_files");
  const listing = toolNames.join(", ");
  const lines = [
    `Nova tools (${listing}) are provided by the nova-tools MCP server in addition to the built-in tools.`
      + " In your tool list their names may carry a server prefix (for example mcp__nova-tools__fast_context or nova-tools_fast_context);"
      + " always invoke the exact names shown in your tool list. The following tool-selection rules are hard constraints.",
    "Prefer minimal reads: when line ranges are known, read only those segments; expand nearby context only as needed. "
      + (fastContext
        ? "When edit distribution is unknown and you need complete cross-file context, call fast_context once (or find_symbols for definitions/references only). Never re-read shown ranges; read SIG/IMPACT bodies by path:line only when truly needed. "
        : "When location is unknown, search first (see below), then read near hits. ")
      + "Do not dump large files blindly."
      + (readOnly
        ? ""
        : " For multi-file changes prefer edit_files in one call; use built-in write/edit for single spots."),
    (fastContext
      ? "Search and traversal must be cost-bounded. If path and range are known, read directly. When edit distribution is unknown, call fast_context once: it returns complete EDIT/DEPS units plus IMPACT/SIG indexes using batched rg and an incremental symbol index; use find_symbols for locations only. Never re-read shown ranges. Read SIG/IMPACT bodies by exact path:line only when truly needed. Do not re-discover the same keywords with shell rg/git grep or built-in grep, and do not re-call merely with a larger budget. Do not use `grep -r` or `grep -R` for unscoped recursive searches of a repo/source root. Fallback searches must honor `.gitignore` by default. "
      : "Search and traversal must be cost-bounded. Do not use `grep -r` or `grep -R` for unscoped recursive searches of a repo/source root. Prefer `rg` (honors `.gitignore`); use `git grep` only as a fallback for tracked-only searches. ")
      + "Unless the task requires it, do not scan build artifacts, dependencies, caches, generated files, or large binary asset dirs. `| head` / `| tail` and output truncation only limit display, not work; recursive commands must narrow via path/glob/type/excludes and use a short timeout. After a recursive timeout, do not retry the same command unchanged—narrow scope or switch tools.",
  ];
  if (readOnly) {
    lines.push("Current mode is plan/read-only: analyze only; do not modify files.");
  }
  return lines.join("\n");
}

/** Prefix the first text part (opencode injects policy once per new session). */
export function prefixFirstTextPart(parts, prefix) {
  const index = (parts ?? []).findIndex((part) => part?.type === "text");
  if (index < 0 || !prefix) return parts;
  const next = [...parts];
  next[index] = { ...next[index], text: `${prefix}\n\n${next[index].text ?? ""}` };
  return next;
}
