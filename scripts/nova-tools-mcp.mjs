/**
 * Nova batch tools MCP stdio server for Devin ACP (session/new mcpServers).
 * Tools: optional fast_context/find_symbols, optional edit_files.
 *
 * Env:
 *   NOVA_FAST_CONTEXT=0     — omit fast_context / find_symbols
 *   NOVA_EDIT_FILES=1       — opt back in to edit_files (default: omitted for all agents)
 *   NOVA_TOOLS_READ_ONLY=1  — plan mode (never exposes edit_files)
 *   NOVA_TOOLS_CWD          — override tool root (default: process.cwd())
 */
import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { CallToolRequestSchema, ListToolsRequestSchema } from "@modelcontextprotocol/sdk/types.js";
import { createNovaBatchTools } from "./nova-batch-tools.mjs";

const cwd = process.env.NOVA_TOOLS_CWD || process.cwd();
// edit_files 全链路默认禁用（Devin / Claude / CodeBuddy 均不注入）；NOVA_EDIT_FILES=1 恢复。
const tools = createNovaBatchTools(cwd, {
  includeEditFiles: process.env.NOVA_EDIT_FILES === "1",
});

const server = new Server(
  { name: "nova-tools", version: "1.0.0" },
  { capabilities: { tools: {} } },
);

server.setRequestHandler(ListToolsRequestSchema, async () => ({
  tools: Object.entries(tools).map(([name, tool]) => ({
    name,
    description: tool.description,
    inputSchema: tool.inputSchema,
  })),
}));

server.setRequestHandler(CallToolRequestSchema, async ({ params }) => {
  const tool = tools[params.name];
  if (!tool) {
    return {
      isError: true,
      content: [{ type: "text", text: `Unknown tool: ${params.name}` }],
    };
  }
  try {
    const text = await tool.execute(params.arguments ?? {});
    return { content: [{ type: "text", text: String(text ?? "") }] };
  } catch (error) {
    return {
      isError: true,
      content: [{ type: "text", text: error instanceof Error ? error.message : String(error) }],
    };
  }
});

await server.connect(new StdioServerTransport());
