import assert from "node:assert/strict";

process.env.NOVA_CODEBUDDY_BRIDGE_TEST = "1";
const {
  assistantItems,
  assistantText,
  codeBuddyBatchToolPolicy,
  novaToolsMcpServers,
  permissionModeFor,
  promptMessages,
  resolveCodeBuddyCliPath,
  streamEventItem,
} = await import("./codebuddy-bridge.mjs");

const npmShim = "C:\\Users\\test\\AppData\\Roaming\\npm\\codebuddy.cmd";
const npmCli = "C:\\Users\\test\\AppData\\Roaming\\npm\\node_modules\\@tencent-ai\\codebuddy-code\\bin\\codebuddy";
assert.equal(resolveCodeBuddyCliPath(npmShim, (path) => path === npmCli), npmCli);
assert.equal(resolveCodeBuddyCliPath(npmShim, () => false), npmShim);
assert.equal(resolveCodeBuddyCliPath("C:\\codebuddy\\codebuddy.exe", () => true), "C:\\codebuddy\\codebuddy.exe");

assert.equal(permissionModeFor("build"), "bypassPermissions");
assert.equal(permissionModeFor("bypass"), "bypassPermissions");
assert.equal(permissionModeFor("plan"), "plan");

const batchPolicy = codeBuddyBatchToolPolicy({ mode: "build" }, { NOVA_FAST_CONTEXT: "1" });
assert.match(batchPolicy, /Nova MCP tools fast_context and find_symbols/);
assert.match(batchPolicy, /call nova-tools fast_context first/);
assert.match(batchPolicy, /Do not re-discover the same keywords/);
assert.doesNotMatch(batchPolicy, /plan\/read-only/);
assert.match(codeBuddyBatchToolPolicy({ mode: "plan" }, { NOVA_FAST_CONTEXT: "1" }), /plan\/read-only/);
const noFastContextPolicy = codeBuddyBatchToolPolicy({ mode: "build" }, { NOVA_FAST_CONTEXT: "0" });
assert.doesNotMatch(noFastContextPolicy, /Nova MCP tools/);
assert.match(noFastContextPolicy, /cost-bounded search/);

const messages = [];
for await (const message of promptMessages({
  parts: [
    { type: "text", text: "inspect" },
    { type: "local_image", path: "C:/Users/1/Desktop/report.xlsx" },
  ],
})) messages.push(message);

assert.deepEqual(messages[0].message.content, [
  { type: "text", text: "inspect" },
  { type: "text", text: "Attached file: C:/Users/1/Desktop/report.xlsx" },
]);

const stream = { messageId: "message", blocks: new Map() };
assert.equal(streamEventItem({
  event: { type: "message_start", message: { id: "message-1" } },
}, stream), null);
assert.equal(streamEventItem({
  event: { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } },
}, stream), null);
assert.deepEqual(streamEventItem({
  event: { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "残" } },
}, stream), { id: "message-1-0", type: "agent_message", text: "残" });

const finalItems = assistantItems({
  message: {
    // The SDK may use IDs in the final snapshot that differ from message_start.
    id: "final-message-id",
    content: [
      { id: "final-text-block", type: "text", text: "完整回答" },
      { id: "final-thinking-block", type: "thinking", thinking: "完整思考" },
      { id: "tool-1", type: "tool_use", name: "Grep", input: { pattern: "榜单" } },
    ],
  },
}, stream);
assert.deepEqual(finalItems, [
  { id: "message-1-0", type: "agent_message", text: "完整回答" },
  { id: "final-thinking-block", type: "reasoning", text: "完整思考" },
  { id: "tool-1", type: "mcp_tool_call", server: "CodeBuddy", tool: "Grep", arguments: { pattern: "榜单" }, status: "in_progress" },
]);
assert.equal(finalItems[0].id, "message-1-0", "the final snapshot must replace the partial item");
assert.equal(assistantText({
  message: { content: [{ type: "thinking", thinking: "ignore" }, { type: "text", text: "修复标题" }] },
}), "修复标题");

const { mkdir, mkdtemp, writeFile } = await import("node:fs/promises");
const { tmpdir } = await import("node:os");
const { join } = await import("node:path");

const novaHome = await mkdtemp(join(tmpdir(), "nova-codebuddy-mcp-"));
assert.equal(novaToolsMcpServers({ cwd: "D:/repo", mode: "build" }, { NOVA_DATA_DIR: join(novaHome, "missing") }), undefined);
await mkdir(join(novaHome, "runtime"), { recursive: true });
await writeFile(join(novaHome, "runtime", "nova-tools-mcp.mjs"), "// stub\n");
const planServers = novaToolsMcpServers({ cwd: "D:/repo", mode: "plan" }, {
  NOVA_DATA_DIR: novaHome,
  NOVA_FAST_CONTEXT: "0",
  NOVA_CONTEXT_SERVICE_ENDPOINT: "http://127.0.0.1:9",
  NOVA_CONTEXT_SERVICE_TOKEN: "token",
});
assert.equal(planServers["nova-tools"].command, process.execPath);
assert.deepEqual(planServers["nova-tools"].args, [join(novaHome, "runtime", "nova-tools-mcp.mjs")]);
assert.deepEqual(planServers["nova-tools"].env, {
  NOVA_TOOLS_CWD: "D:/repo",
  NOVA_FAST_CONTEXT: "0",
  NOVA_TOOLS_READ_ONLY: "1",
  NOVA_CONTEXT_SERVICE_ENDPOINT: "http://127.0.0.1:9",
  NOVA_CONTEXT_SERVICE_TOKEN: "token",
});
const buildServers = novaToolsMcpServers({ cwd: "/repo", mode: "build" }, { NOVA_DATA_DIR: novaHome });
assert.equal(buildServers["nova-tools"].env.NOVA_TOOLS_READ_ONLY, undefined);
assert.equal(buildServers["nova-tools"].env.NOVA_FAST_CONTEXT, "1");
