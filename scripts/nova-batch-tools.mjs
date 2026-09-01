import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { POLARIS_DESCRIPTION } from "./ctx-core.mjs";
import { callGlobalContextTool, globalContextServiceConfigured } from "./nova-context-client.mjs";

function browserDebugEnabled(options = {}) {
  if (typeof options.browserDebug === "boolean") return options.browserDebug;
  return process.env.NOVA_BROWSER_DEBUG === "1";
}

async function browserMcpPort() {
  const file = process.env.NOVA_BROWSER_MCP_PORT_FILE;
  if (!file) throw new Error("NOVA_BROWSER_MCP_PORT_FILE 未配置，browser 工具不可用");
  const raw = await readFile(file, "utf8");
  const port = Number.parseInt(raw.trim(), 10);
  if (!Number.isInteger(port) || port <= 0) {
    throw new Error("Nova browser 中转端口不可用，请先在 Lyra 中发送 /browser 打开浏览器");
  }
  return port;
}

async function callBrowserTool(command) {
  const port = await browserMcpPort();
  const response = await fetch(`http://127.0.0.1:${port}/`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(command),
    signal: AbortSignal.timeout(35_000),
  });
  const payload = await response.json().catch(() => ({}));
  if (!response.ok || payload.ok !== true) {
    throw new Error(payload.error || `browser 中转返回 HTTP ${response.status}`);
  }
  return payload.data;
}

const BROWSER_DESCRIPTION =
  "通过 Nova 复用双子座的 Playwright 进程进行持续的前端开发与调试。可打开/跳转网站、交互、查看 console/pageerror/失败请求与 HTTP 错误，并截图。每个会话使用独立且跨轮次保留的标签页；仅在用户要求或退出调试模式时 close。";

function browserInputSchema() {
  return {
    type: "object",
    properties: {
      operation: { type: "string", enum: ["open", "goto", "inspect", "screenshot", "act", "close"] },
      url: { type: "string", description: "open/goto 的网址；localhost 默认补 http://" },
      headless: { type: "boolean", description: "open 是否无头，默认 false" },
      action: { type: "string", enum: ["click", "fill", "press", "type", "scroll", "wait"], description: "operation=act 时的操作" },
      selector: { type: "string", description: "CSS selector；用于交互、等待或元素截图" },
      text: { type: "string" },
      limit: { type: "integer" },
      clear: { type: "boolean" },
      fullPage: { type: "boolean" },
      timeout: { type: "integer" },
    },
    required: ["operation"],
    additionalProperties: false,
  };
}

function browserCommand(params, sessionId) {
  const operation = String(params.operation ?? "");
  const base = { sessionId };
  switch (operation) {
    case "open": return { ...base, cmd: "devOpen", url: params.url, headless: params.headless };
    case "goto": return { ...base, cmd: "devGoto", url: params.url };
    case "inspect": return { ...base, cmd: "devInspect", limit: params.limit, clear: params.clear };
    case "screenshot": return { ...base, cmd: "devScreenshot", selector: params.selector, fullPage: params.fullPage, timeout: params.timeout };
    case "act": return { ...base, cmd: "devAct", action: params.action, selector: params.selector, text: params.text };
    case "close": return { ...base, cmd: "devClose" };
    default: throw new Error(`browser operation 无效：${operation}`);
  }
}

function fastContextEnabled(options = {}) {
  const enabled = typeof options.fastContext === "boolean"
    ? options.fastContext
    : process.env.NOVA_FAST_CONTEXT !== "0";
  return enabled && globalContextServiceConfigured();
}

function readOnlyEnabled(options = {}) {
  if (typeof options.readOnly === "boolean") return options.readOnly;
  return process.env.NOVA_TOOLS_READ_ONLY === "1";
}

function stringList(value) {
  const list = Array.isArray(value) ? value : typeof value === "string" ? [value] : [];
  return [...new Set(list.map((item) => String(item ?? "").trim()).filter(Boolean))];
}

export function normalizePolarisArgs(params = {}) {
  const query = String(params.query ?? "").trim();
  const keywords = stringList(params.keywords).slice(0, 5);
  const task = String(params.task ?? "").trim() || (query.includes(" ") ? query : "");
  if (!keywords.length && query && !task) keywords.push(query);
  const files = stringList(params.files).slice(0, 6);
  return {
    ...params,
    keywords,
    task,
    files,
  };
}


/**
 * Shared Nova context tools for Cursor customTools and Devin ACP MCP.
 * @param {string} cwd
 * @param {{ readOnly?: boolean, fastContext?: boolean }} [options]
 */
export function createNovaBatchTools(cwd, options = {}) {
  const fastContext = fastContextEnabled(options);
  const browserDebug = browserDebugEnabled(options);
  readOnlyEnabled(options);
  const root = resolve(cwd);
  const browserSessionId = `nova-mcp-${process.pid}`;

  /** @type {Record<string, { description: string, inputSchema: object, execute: (args: any) => Promise<string> }>} */
  const tools = {};

  if (browserDebug) {
    tools.browser = {
      description: BROWSER_DESCRIPTION,
      inputSchema: browserInputSchema(),
      async execute(params) {
        const data = await callBrowserTool(browserCommand(params ?? {}, browserSessionId));
        if (params?.operation === "screenshot" && data?.image) {
          return `[browser screenshot saved: ${data.path ?? "(inline)"}] ${data.url ?? ""}`;
        }
        return typeof data === "string" ? data : JSON.stringify(data);
      },
    };
  }

  if (fastContext) {
    tools.polaris = {
      description: process.env.NOVA_MCP_DIRECT === "1"
        ? `Nova 上下文工具。需要跨文件查找、分析修改位置或读取多个文件时直接调用，禁止经过任何中间工具。${POLARIS_DESCRIPTION} 必须提供 keywords、query、task 或 files 至少一个；例如 {"query":"cursor bridge"}。`
        : `远程 MCP 端点，只能通过 Devin 顶层 mcp_call_tool 调用：server_name="nova-tools", tool_name="polaris"；禁止把 polaris 当作顶层工具直接调用。${POLARIS_DESCRIPTION} 必须提供 keywords、query、task 或 files 至少一个；例如 {"query":"cursor bridge"}。`,
      inputSchema: {
        type: "object",
        properties: {
          keywords: {
            anyOf: [
              { type: "array", minItems: 1, items: { type: "string", minLength: 1 } },
              { type: "string", minLength: 1 },
            ],
            description: "关键词或符号名；字符串自动转单项数组，超过 5 项默认取前 5 项",
          },
          query: { type: "string", minLength: 1, description: "简短检索词；兼容单字符串调用，如 cursor" },
          task: { type: "string", minLength: 1, description: "自然语言任务描述，将自动提取检索词" },
          files: { type: "array", minItems: 1, items: { type: "string", minLength: 1 }, description: "明确要纳入上下文的仓库相对文件路径" },
          maxChars: { type: "integer", minimum: 4000, maximum: 80000, description: "输出字符预算，通常无需设置" },
          budget: { type: "integer", minimum: 100, maximum: 4000, description: "兼容旧参数：行预算，通常无需设置" },
          coupling: { type: "boolean", description: "开启后附 git 共改耦合提示（近 120 次提交的高频共改文件）" },
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
        const args = normalizePolarisArgs(params);
        return await callGlobalContextTool("polaris", root, args);
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
  const toolNames = [];
  if (fastContext) toolNames.push("polaris");
  if (toolNames.length === 0) {
    const lines = ["Nova MCP server nova-tools exposes no tools in this mode; use Devin built-in tools."];
    if (readOnly) lines.push("Current mode is plan/read-only: analyze only; do not modify files.");
    return lines.join("\n");
  }
  const example = '{"server_name":"nova-tools","tool_name":"polaris","arguments":{"query":"cursor"}}';
  const novaToolsPhrase = toolNames.length
    ? `You have Nova MCP endpoints from server nova-tools (${toolNames.join(", ")}) plus Devin built-in tools. In this Devin version, ${toolNames.join(", ")} are remote MCP tool names, NOT top-level callable Devin tools.`
    : "Nova MCP server nova-tools exposes no tools in this mode; use Devin built-in tools.";
  const callExampleName = "polaris";
  const lines = [
    `ROUTING RULE — before choosing any tool: Nova endpoints must NEVER be selected as direct tool calls. Select Devin's top-level mcp_call_tool first, then pass server_name="nova-tools" and the endpoint name in tool_name. ${novaToolsPhrase} Never select or invoke any of those names directly, even after mcp_list_tools lists them; a direct invocation produces \`Unknown tool ... This tool is not available.\` Your only valid execution path for a Nova tool is Devin's generic mcp_call_tool wrapper. Set server_name to the top-level string "nova-tools" (never omit it or put it inside arguments), and put only the selected Nova tool's inputs in arguments. Example: ${example}. Follow the wrapper's declared tool-name field if its schema uses a different spelling. The available Nova tools are already stated above; do not call mcp_list_tools merely to discover them. In every rule below, wording such as \`use/call ${callExampleName}\` means \`call mcp_call_tool with server_name nova-tools and tool_name ${callExampleName}\`; it never authorizes a direct tool call. If a direct call reports \`Unknown tool\`, retry once through mcp_call_tool. If parsing reports missing field \`server_name\`, correct the wrapper call once. Never repeat a malformed call unchanged. The following tool-selection rules are hard constraints.`,
    "Prefer minimal reads via Devin native read: when line ranges are known, read only those segments; expand nearby context only as needed. "
      + (fastContext
        ? "When location is unknown, you must call only polaris; then read only coverage gaps / next_reads with native read. "
        : "When location is unknown, search first (see below), then read near hits. ")
      + "Do not dump large files blindly."
      + " For edits, use Devin native edit tools. Multiple edits for the same file must be merged into one native edit call.",
    (fastContext
      ? "Search and traversal must be cost-bounded. When symbol/keyword distribution or surrounding code is unknown, you MUST call only polaris (packs definition bodies + 1-hop neighbors + coverage; internal rg, honors `.gitignore`). Do not re-read FULL/BODY.covered ranges; fill gaps via next_reads with Devin native read. After polaris, do not re-discover the same keywords with shell `rg`/`git grep` or Devin grep—rg is already inside polaris. External rg/grep/git grep are allowed only when: (1) next_reads/gaps are still insufficient, or (2) the task explicitly needs a scoped literal search that polaris did not cover. Do not use `grep -r` or `grep -R` for unscoped recursive searches of a repo/source root. Fallback searches must honor `.gitignore` by default. "
      : "Search and traversal must be cost-bounded. Do not use `grep -r` or `grep -R` for unscoped recursive searches of a repo/source root. Prefer `rg` (honors `.gitignore`); use `git grep` only as a fallback for tracked-only searches. ")
      + "Unless the task requires it, do not scan build artifacts, dependencies, caches, generated files, or large binary asset dirs. `| head` / `| tail` and output truncation only limit display, not work; recursive commands must narrow via path/glob/type/excludes and use a short timeout. After a recursive timeout, do not retry the same command unchanged—narrow scope or switch tools.",
  ];
  if (readOnly) {
    lines.push("Current mode is plan/read-only: analyze only; do not modify files.");
  }
  return lines.join("\n");
}
