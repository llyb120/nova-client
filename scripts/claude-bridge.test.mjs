import assert from "node:assert/strict";

process.env.NOVA_CLAUDE_BRIDGE_TEST = "1";
const { assistantItems, claudeModelOptions, claudeModelSelection, novaToolsMcpServers, promptText, streamEventItem } = await import("./claude-bridge.mjs");

assert.deepEqual(claudeModelOptions([
  {
    value: "sonnet",
    displayName: "Sonnet",
    description: "Balanced",
    supportsEffort: true,
    supportedEffortLevels: ["low", "high", "max"],
  },
  { value: "haiku", displayName: "Haiku", description: "Fast" },
]), [
  { value: "sonnet:low", name: "Sonnet Low", description: "Balanced" },
  { value: "sonnet:high", name: "Sonnet High", description: "Balanced" },
  { value: "sonnet:max", name: "Sonnet Max", description: "Balanced" },
  { value: "haiku", name: "Haiku", description: "Fast" },
]);
assert.deepEqual(claudeModelSelection("sonnet:xhigh"), { model: "sonnet", effort: "xhigh" });
assert.deepEqual(claudeModelSelection("opus[1m]"), { model: "opus[1m]" });
assert.equal(promptText([
  { type: "text", text: "inspect" },
  { type: "local_image", path: "C:\\Users\\1\\Desktop\\质量.xlsx" },
]), "inspect\n\nAttached file: C:\\Users\\1\\Desktop\\质量.xlsx");

const stream = { messageId: "message", blocks: new Map() };
const streamedBlocks = new Set();
assert.equal(streamEventItem({ event: { type: "message_start", message: { id: "msg" } } }, stream, streamedBlocks), null);
assert.deepEqual(streamEventItem({ event: {
  type: "content_block_start",
  index: 0,
  content_block: { type: "text", text: "Hello" },
} }, stream, streamedBlocks), { id: "msg-0", type: "agent_message", text: "Hello" });
assert.deepEqual(streamEventItem({ event: {
  type: "content_block_delta",
  index: 0,
  delta: { type: "text_delta", text: " world" },
} }, stream, streamedBlocks), { id: "msg-0", type: "agent_message", text: "Hello world" });
assert.deepEqual(streamEventItem({ event: {
  type: "content_block_delta",
  index: 1,
  delta: { type: "thinking_delta", thinking: "Checking" },
} }, stream, streamedBlocks), { id: "msg-1", type: "reasoning", text: "Checking" });
assert.deepEqual(assistantItems({
  uuid: "final",
  message: { content: [
    { type: "text", text: "Hello world" },
    { type: "thinking", thinking: "Checking" },
    { type: "tool_use", id: "tool", name: "Read", input: { path: "a" } },
  ] },
}, streamedBlocks), [{
  id: "tool",
  type: "mcp_tool_call",
  server: "Claude",
  tool: "Read",
  arguments: { path: "a" },
  status: "in_progress",
}]);

const { mkdir, mkdtemp, writeFile } = await import("node:fs/promises");
const { tmpdir } = await import("node:os");
const { join } = await import("node:path");

const novaHome = await mkdtemp(join(tmpdir(), "nova-claude-mcp-"));
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
