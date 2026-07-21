import { Agent } from "@earendil-works/pi-agent-core";
import { createCodingTools, createReadOnlyTools, getShellConfig } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { createReadStream } from "node:fs";
import { readFile, readdir } from "node:fs/promises";
import { homedir } from "node:os";
import { extname, isAbsolute, join, relative, resolve } from "node:path";
import { createInterface } from "node:readline";

const DEFAULT_BATCH_READ_LINES = 200;
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
      description: `优先用于一次读取两个及以上路径已知的 UTF-8 文本文件；内部并行、流式读取，默认每个文件读取前 ${DEFAULT_BATCH_READ_LINES} 行。可为每个文件指定 offset/limit，并用返回的 nextOffset 继续读取。`,
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

async function findSkills(roots) {
  const skills = [];
  const seen = new Set();
  for (const root of roots) {
    for (const entry of await readdir(root, { withFileTypes: true }).catch(() => [])) {
      if (!entry.isDirectory() || seen.has(entry.name)) continue;
      const path = join(root, entry.name, "SKILL.md");
      const markdown = await readFile(path, "utf8").catch(() => null);
      if (markdown == null) continue;
      seen.add(entry.name);
      const description = markdown.match(/^description:\s*(.+)$/mi)?.[1]?.trim() ?? "";
      skills.push({ name: entry.name, description, path });
    }
  }
  return skills;
}

export async function createSkillSupport(root = join(homedir(), ".nova", "alkaid", "skills")) {
  const skills = await findSkills([root]);
  const byName = new Map(skills.map((skill) => [skill.name, skill]));
  const tool = {
    name: "load_skill",
    description: "按名称加载相关 Skill 的完整 SKILL.md。仅在任务匹配时调用。",
    parameters: Type.Object({ name: Type.String() }),
    async execute(_id, { name }) {
      const skill = byName.get(name);
      if (!skill) throw new Error(`未知 skill: ${name}`);
      return textResult(await readFile(skill.path, "utf8"), { name });
    },
  };
  const catalog = skills.length
    ? skills.map(({ name, description }) => `- ${name}: ${description || "无描述"}`).join("\n")
    : "（当前未安装 Skill）";
  return { skills, catalog, tool };
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
  const skillSupport = await createSkillSupport();
  const mcp = await connectMcpServers(options.mcpServers, cwd);
  const shellConfig = options.readOnly ? null : (options.shellConfig ?? getShellConfig());
  const codingTools = options.readOnly
    ? createReadOnlyTools(cwd)
    : createCodingTools(cwd, { bash: { shellPath: shellConfig.shell } });
  const editTool = codingTools.find((tool) => tool.name === "edit");
  const batchTools = createFilesystemTools(cwd, editTool);
  const tools = [...batchTools, ...codingTools, skillSupport.tool, ...mcp.tools];
  const systemPrompt = [
    "你是 Alkaid：高效、简单、面向软件工程结果。",
    "你拥有批量增强 read_files、edit_files，以及 PI coding agent 的原生 read、bash、edit、write 工具。读取两个及以上路径已知、互不依赖的文本文件时必须优先使用 read_files，不要连续调用多个单文件 read；只有目标路径依赖前一次结果，或内容不是 UTF-8 文本时才使用原生 read。修改两个及以上互不依赖的已有文件时必须优先使用 edit_files；同一文件的多处修改合并到该文件的一组 edits。读取大文件时使用 offset/limit 分段；仅在存在先后依赖或目标重叠时串行调用工具。",
    "先理解再修改，保持改动聚焦；完成后简洁报告结果和验证。",
    shellConfig ? `命令终端已确认使用 Bash（${shellConfig.shell}）；bash 工具必须从第一次调用起使用 Bash 语法，不要使用 PowerShell cmdlet。` : "",
    options.readOnly ? "当前为计划模式：只读分析，不得修改文件。" : "",
    `工作区：${cwd}`,
    "可用 Skills（需要时先调用 load_skill）：",
    skillSupport.catalog,
    options.systemPrompt ?? "",
  ].filter(Boolean).join("\n\n");
  const agent = new Agent({
    initialState: {
      systemPrompt,
      model: options.model,
      thinkingLevel: options.thinkingLevel,
      tools,
      messages: options.messages ?? [],
    },
    getApiKey: () => options.apiKey,
    toolExecution: "parallel",
    steeringMode: "all",
    sessionId: options.sessionId,
  });
  return { agent, close: () => mcp.close(), skills: skillSupport.skills, toolCount: tools.length };
}
