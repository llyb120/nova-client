import imageTools from "./image-tools.json" with { type: "json" };
import chromeTool from "./chrome-tool.json" with { type: "json" };
import jianlaiTool from "./jianlai-tool.json" with { type: "json" };
import employeeTool from "./employee-tool.json" with { type: "json" };
import agentTools from "./agent-tools.json" with { type: "json" };
import { resolve } from "node:path";
import { randomUUID } from "node:crypto";
import { POLARIS_DESCRIPTION } from "./ctx-core.mjs";
import { callGlobalContextTool, globalContextServiceConfigured } from "./nova-context-client.mjs";

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
 * Shared Nova context tools for MCP backends.
 * @param {string} cwd
 * @param {{ readOnly?: boolean, fastContext?: boolean, toolEnv?: Record<string, string> }} [options]
 */
export function createNovaBatchTools(cwd, options = {}) {
  const fastContext = fastContextEnabled(options);
  const readOnly = readOnlyEnabled(options);
  const root = resolve(cwd);
  // 自动模式（Nova 按会话注入）：当前模型不是剑来场景的模型时不暴露桌面/浏览器工具；
  // 存在模型不同的场景时可按场景转交。
  const env = (name) => String(options.toolEnv?.[name] ?? process.env[name] ?? "");
  const threadId = env("NOVA_THREAD_ID");
  // 已知会话时以它为 owner：Nova 据此认出数字员工任务的桌面操作与待确认来源。
  const owner = threadId ? `thread:${threadId}` : randomUUID();
  const desktop = env("NOVA_DESKTOP") !== "0";

  /** @type {Record<string, { description: string, inputSchema: object, execute: (args: any) => Promise<string> }>} */
  const tools = {};
  if (!readOnly && globalContextServiceConfigured()) {
    if (desktop) {
      tools.jianlai = { ...jianlaiTool,
        execute: async params => JSON.stringify(await callGlobalContextTool("jianlai", root, params, owner)),
      };
    }
    if (threadId && env("NOVA_SCENE_HANDOFF") === "1") {
      for (const { name, description, inputSchema } of agentTools) {
        tools[name] = { description, inputSchema,
          execute: async args => JSON.stringify(await callGlobalContextTool("agent", root, { threadId, name, args })),
        };
      }
    }
    tools.employee = { ...employeeTool,
      execute: async params => JSON.stringify(await callGlobalContextTool("employee", root, params, owner)),
    };
    // 非自动模式保持原工具顺序（jianlai、employee、chrome）。
    if (desktop) {
      tools.chrome = { ...chromeTool,
        execute: async params => JSON.stringify(await callGlobalContextTool("chrome", root, params, owner)),
      };
    }
    for (const definition of imageTools) {
      tools[definition.name] = { ...definition,
        execute: async (params) => JSON.stringify(await callGlobalContextTool(definition.name, root, params)),
      };
    }
  }

  if (fastContext) {
    tools.polaris = {
      description: `Nova 上下文工具。需要跨文件查找、分析修改位置或读取多个文件时调用 polaris。${POLARIS_DESCRIPTION} 必须提供 keywords、query、task 或 files 至少一个；例如 {"query":"context service"}。`,
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
          query: { type: "string", minLength: 1, description: "简短检索词；兼容单字符串调用，如 context" },
          task: { type: "string", minLength: 1, description: "自然语言任务描述，将自动提取检索词" },
          files: { type: "array", minItems: 1, items: { type: "string", minLength: 1 }, description: "明确要纳入上下文的仓库相对文件路径" },
          maxChars: { type: "integer", minimum: 4000, maximum: 80000, description: "输出字节硬上限（兼容 maxBytes），默认 12288（12KB），不凑满；通常无需设置" },
          budget: { type: "integer", minimum: 100, maximum: 4000, description: "兼容旧参数：输出行预算，通常无需设置，不用于凑满输出" },
          deep: { type: "boolean", description: "默认 false，仅返回快速概览；确需递归依赖、别名调用及伴生测试时才开启（较慢）" },
          coupling: { type: "boolean", description: "开启后附 git 共改耦合提示（近 120 次提交的高频共改文件）" },
        },
        // 不用顶层 anyOf 表达"至少其一"：Anthropic API 不接受，Claude Code 会直接丢弃该工具；由描述和服务端兜底。
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
