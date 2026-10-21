/**
 * Nova context tools MCP stdio server for Devin ACP (session/new mcpServers).
 * Tools: optional polaris.
 *
 * Env:
 *   NOVA_FAST_CONTEXT=0     — omit polaris
 *   NOVA_TOOLS_CWD          — override tool root (default: process.cwd())
 */
import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { CallToolRequestSchema, ListToolsRequestSchema } from "@modelcontextprotocol/sdk/types.js";
import { createNovaBatchTools, listNovaBatchTools } from "./nova-batch-tools.mjs";
import { automationMcpResult } from "./automation-mcp-result.mjs";

const cwd = process.env.NOVA_TOOLS_CWD || process.cwd();
const tools = createNovaBatchTools(cwd);

const server = new Server(
  { name: "nova-tools", version: "1.0.0" },
  { capabilities: { tools: {} } },
);

server.setRequestHandler(ListToolsRequestSchema, async () => ({
  tools: await listNovaBatchTools(tools, cwd),
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
    if (["chrome", "jianlai"].includes(params.name)) return await automationMcpResult(String(text ?? ""));
    return { content: [{ type: "text", text: String(text ?? "") }] };
  } catch (error) {
    return {
      isError: true,
      content: [{ type: "text", text: error instanceof Error ? error.message : String(error) }],
    };
  }
});

await server.connect(new StdioServerTransport());
