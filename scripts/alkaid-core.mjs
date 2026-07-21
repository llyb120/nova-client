import { Agent } from "@earendil-works/pi-agent-core";
import { createCodingTools, createReadOnlyTools, getShellConfig } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { createReadStream } from "node:fs";
import { readFile, readdir, rename, stat, unlink, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { basename, dirname, isAbsolute, join, relative, resolve } from "node:path";
import { createInterface } from "node:readline";

const DEFAULT_BATCH_READ_LINES = 200;

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

export function createFilesystemTools(cwd) {
  const root = resolve(cwd);
  return [
    {
      name: "read_files",
      description: `并行、流式读取多个 UTF-8 文本文件，默认每个文件读取前 ${DEFAULT_BATCH_READ_LINES} 行。可为每个文件指定 offset/limit，并用返回的 nextOffset 继续读取。`,
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
    {
      name: "write_files",
      description: "并行、原子地写入多个工作区文件。仅用于彼此独立的目标文件；同一路径不可重复。",
      parameters: Type.Object({
        files: Type.Array(Type.Object({ path: Type.String(), content: Type.String() }), { minItems: 1 }),
      }),
      async execute(_id, { files }) {
        const targets = files.map((file) => safePath(root, file.path));
        if (new Set(targets).size !== targets.length) throw new Error("write_files 包含重复目标路径");
        const written = await Promise.all(files.map(async (file, index) => {
          const target = targets[index];
          const parent = dirname(target);
          const parentInfo = await stat(parent).catch(() => null);
          if (!parentInfo?.isDirectory()) throw new Error(`父目录不存在: ${relative(root, parent)}`);
          const temp = join(parent, `.${basename(target)}.nova-${process.pid}-${index}.tmp`);
          try {
            await writeFile(temp, file.content, "utf8");
            await rename(temp, target);
            return file.path;
          } catch (error) {
            await unlink(temp).catch(() => {});
            throw error;
          }
        }));
        return textResult(`已并行写入 ${written.length} 个文件`, { paths: written });
      },
    },
  ];
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
  const batchTools = createFilesystemTools(cwd).filter((tool) => !options.readOnly || tool.name !== "write_files");
  const tools = [...codingTools, ...batchTools, skillSupport.tool, ...mcp.tools];
  const systemPrompt = [
    "你是 Alkaid：高效、简单、面向软件工程结果。",
    "你拥有 PI coding agent 的原生 read、bash、edit、write 工具，以及批量增强 read_files、write_files；读取大文件时使用 offset/limit 分段，互不依赖的工具调用应在同一轮并发发出。",
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
    sessionId: options.sessionId,
  });
  return { agent, close: () => mcp.close(), skills: skillSupport.skills, toolCount: tools.length };
}
