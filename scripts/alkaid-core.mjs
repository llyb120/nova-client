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
import { createReadStream } from "node:fs";
import { readFile } from "node:fs/promises";
import { homedir } from "node:os";
import { dirname, extname, isAbsolute, join, relative, resolve } from "node:path";
import { createInterface } from "node:readline";

const DEFAULT_BATCH_READ_LINES = 200;
const DEFAULT_PROVIDER_RETRY_DELAYS_MS = [1000, 3000];
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

function safePath(root, input) {
  const path = resolve(root, input);
  const rel = relative(root, path);
  if (rel === ".." || rel.startsWith(`..${process.platform === "win32" ? "\\" : "/"}`) || isAbsolute(rel)) {
    throw new Error(`路径超出工作区: ${input}`);
  }
  return path;
}

export function alkaidSkillsRoot(home = homedir()) {
  return join(home, ".nova", "alkaid", "skills");
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

export function isRetryableAlkaidProviderError(error) {
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
  ].some((fragment) => message.includes(fragment));
}

const wait = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

export async function runAlkaidPromptWithRetry(agent, input, images, options = {}) {
  const retryDelaysMs = options.retryDelaysMs ?? DEFAULT_PROVIDER_RETRY_DELAYS_MS;
  const sleep = options.sleep ?? wait;
  const isCancelled = options.isCancelled ?? (() => false);
  let retries = 0;
  await agent.prompt(input, images);
  while (true) {
    const last = agent.state.messages.at(-1);
    if (last?.role !== "assistant" || last.stopReason !== "error") {
      return { last, retries, cancelled: last?.role === "assistant" && last.stopReason === "aborted" };
    }
    if (retries >= retryDelaysMs.length || !isRetryableAlkaidProviderError(last.errorMessage)) {
      return { last, retries, cancelled: false };
    }
    if (isCancelled()) return { last, retries, cancelled: true };
    agent.state.messages = agent.state.messages.slice(0, -1);
    options.onRetry?.({ attempt: retries + 1, error: last.errorMessage });
    await sleep(retryDelaysMs[retries]);
    retries += 1;
    if (isCancelled()) return { last: agent.state.messages.at(-1), retries, cancelled: true };
    await agent.continue();
  }
}

async function readTextLines(path, offset = 1, limit = DEFAULT_BATCH_READ_LINES) {
  const input = createReadStream(path, { encoding: "utf8" });
  const lines = createInterface({ input, crlfDelay: Infinity });
  const content = [];
  let lineNumber = 0;
  let truncated = false;
  try {
    for await (const line of lines) {
      lineNumber += 1;
      if (lineNumber < offset) continue;
      if (content.length === limit) {
        truncated = true;
        break;
      }
      content.push(line);
    }
  } finally {
    lines.close();
    input.destroy();
  }
  return {
    content: content.join("\n"),
    truncated,
    nextOffset: truncated ? offset + content.length : undefined,
  };
}

export function createFilesystemTools(cwd, editTool = null) {
  const root = resolve(cwd);
  const tools = [
    {
      name: "read_files",
      description: `同一读取阶段已有两个及以上路径已知、互不依赖的 UTF-8 文本目标时必须调用一次本工具，不得拆成多个 read；内部并行、流式读取，默认每个文件读取前 ${DEFAULT_BATCH_READ_LINES} 行。请为每个文件按需指定 offset/limit，并用返回的 nextOffset 继续读取。`,
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
            const path = safePath(root, request.path);
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
      description: "并行精确编辑多个互不依赖的文件。每个文件的 edits 使用与原生 edit 相同的唯一、非重叠 oldText 精确替换；同一路径不可重复。",
      parameters: Type.Object({
        files: Type.Array(Type.Object({
          path: Type.String(),
          edits: Type.Array(Type.Object({ oldText: Type.String(), newText: Type.String() }), { minItems: 1 }),
        }), { minItems: 1 }),
      }),
      async execute(id, { files }, signal) {
        const targets = files.map((file) => safePath(root, file.path));
        if (new Set(targets).size !== targets.length) throw new Error("edit_files 包含重复目标路径");
        const edited = await Promise.all(files.map(async (file, index) => {
          await editTool.execute(`${id}-${index}`, file, signal);
          return file.path;
        }));
        return textResult(`已并行编辑 ${edited.length} 个文件`, { paths: edited });
      },
    });
  }
  return tools;
}

/** Load skills via pi-coding-agent discovery (Agent Skills standard). */
export function loadAlkaidSkills(root = alkaidSkillsRoot()) {
  return loadSkillsFromDir({ dir: root, source: "user" });
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

export function buildAlkaidSystemPrompt(options = {}) {
  const cwd = (options.cwd ?? process.cwd()).replace(/\\/g, "/");
  const skills = options.skills ?? [];
  const toolLines = [
    `- read_files: 并行读取多个 UTF-8 文本文件（可带 offset/limit）`,
    options.readOnly ? null : "- edit_files: 并行精确编辑多个互不依赖的已有文件",
    "- read: 读取单个文件",
    options.readOnly ? "- grep / find / ls: 只读搜索与列举" : "- bash: 执行 Bash 命令",
    options.readOnly ? null : "- edit / write: 单文件编辑或写入",
  ].filter(Boolean);

  const stableParts = [
    "你是 Alkaid：高效、简单、面向软件工程结果。",
    `Available tools:\n${toolLines.join("\n")}`,
    "你拥有批量增强 read_files、edit_files，以及 PI coding agent 的原生 read、bash、edit、write 工具。以下工具选择规则是硬性约束。每次准备读取前，先汇总当前已知目标：仅有一个目标时使用 read；同一读取阶段已有两个及以上路径已知、互不依赖的 UTF-8 文本目标时，必须在一次 read_files 调用中合并读取，并为每个文件分别设置必要的 offset/limit。禁止连续调用多个 read，也禁止用并行封装的多个 read 代替 read_files；想按顺序理解文件不构成读取依赖。只有后一个目标的路径或读取范围必须由前一次结果确定、目标不是 UTF-8 文本，或当前确实仅需一个文件时，才使用 read。后续新发现多个独立文本目标时，下一读取阶段仍须合并使用 read_files。读取内容遵循最小必要原则：已知目标行范围时，只读取相关行段；需要更多上下文时再按需读取相邻行段。未知目标位置时，先用搜索工具定位行号，再读取命中位置附近的必要上下文；大文件禁止无目的全量读取。修改两个及以上互不依赖的已有文件时必须使用 edit_files；同一文件的多处修改合并到该文件的一组 edits。仅在存在先后依赖或目标重叠时串行调用工具。",
    "搜索与遍历必须成本有界。禁止使用 `grep -r` 或 `grep -R` 对仓库根目录或源码根目录进行无排除的递归搜索；Git 仓库中搜索已跟踪文件时优先使用 `git grep`，需要搜索未跟踪文件时使用 `rg`，并默认遵守 `.gitignore`。除非任务明确要求，不得扫描构建产物、依赖、缓存、生成文件或大型二进制资源目录。`| head`、`| tail` 和输出截断只限制结果展示，不属于工作量限制；递归命令必须通过限定路径、glob、文件类型或排除目录缩小实际扫描范围，并设置较短的 timeout。递归命令超时后不得原样重试，必须缩小范围或改用更合适的搜索工具。",
    "先理解再修改，保持改动聚焦；完成后简洁报告结果和验证。",
    "完成修改后，必须基于仓库的依赖清单、工作区配置、构建脚本和 CI 配置识别可独立验证的工程单元及其依赖关系，并根据实际改动计算影响范围。对每个受影响单元运行成本最低且有效的检查；公共接口、共享代码、依赖、配置、代码生成或构建流程改动还必须覆盖受影响的使用方。不要预设语言、框架或架构，不得用一个单元的验证代替其他受影响单元。无法确定边界时扩大验证范围；无法执行时如实报告未验证范围、原因、建议命令和剩余风险。",
    options.shellConfig
      ? `命令终端已确认使用 Bash（${options.shellConfig.shell}）；bash 工具必须从第一次调用起使用 Bash 语法，不要使用 PowerShell cmdlet。`
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

export function resolveAlkaidShellConfig(shellConfig, env = process.env) {
  const shim = env.NOVA_SHELL_SHIM_BASH;
  return process.platform === "win32" && shim
    ? { ...shellConfig, shell: shim }
    : shellConfig;
}

function mcpResult(result) {
  const content = (result.content ?? []).flatMap((part) => {
    if (part.type === "text") return [{ type: "text", text: part.text }];
    if (part.type === "image") return [{ type: "image", data: part.data, mimeType: part.mimeType }];
    return [{ type: "text", text: JSON.stringify(part) }];
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
  if (!options.model) throw new Error("Alkaid 缺少模型配置");
  const cwd = resolve(options.cwd ?? process.cwd());
  const { skills } = loadAlkaidSkills(options.skillsRoot ?? alkaidSkillsRoot());
  const mcp = await connectMcpServers(options.mcpServers, cwd);
  const detectedShellConfig = options.readOnly ? null : (options.shellConfig ?? getShellConfig());
  const shellConfig = detectedShellConfig && resolveAlkaidShellConfig(detectedShellConfig);
  const codingTools = options.readOnly
    ? createReadOnlyTools(cwd)
    : createCodingTools(cwd, { bash: { shellPath: shellConfig.shell } });
  const editTool = codingTools.find((tool) => tool.name === "edit");
  const batchTools = createFilesystemTools(cwd, editTool);
  const tools = [...batchTools, ...codingTools, ...mcp.tools];
  const systemPrompt = buildAlkaidSystemPrompt({
    cwd,
    skills,
    readOnly: options.readOnly,
    shellConfig,
    systemPrompt: options.systemPrompt,
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
      return injectOpenAIPromptCacheKey(payload, sessionId);
    },
  });
  return { agent, close: () => mcp.close(), skills, toolCount: tools.length };
}
