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
  toolResultItems,
  completePendingTools,
} = await import("./codebuddy-bridge.mjs");

const npmShim = "C:\\Users\\test\\AppData\\Roaming\\npm\\codebuddy.cmd";
const npmCli = "C:\\Users\\test\\AppData\\Roaming\\npm\\node_modules\\@tencent-ai\\codebuddy-code\\bin\\codebuddy";
assert.equal(resolveCodeBuddyCliPath(npmShim, (path) => path === npmCli), npmCli);
assert.equal(resolveCodeBuddyCliPath(npmShim, () => false), npmShim);
assert.equal(resolveCodeBuddyCliPath("C:\\codebuddy\\codebuddy.exe", () => true), "C:\\codebuddy\\codebuddy.exe");
assert.doesNotThrow(() => resolveCodeBuddyCliPath(npmShim), "default filesystem check must be defined");

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
}, { NOVA_FAST_CONTEXT: "1" })) messages.push(message);

assert.match(messages[0].message.content[0].text, /first repository tool call MUST be mcp__nova-tools__fast_context/);
assert.deepEqual(messages[0].message.content.slice(1), [
  { type: "text", text: "inspect" },
  { type: "text", text: "Attached file: C:/Users/1/Desktop/report.xlsx" },
]);

const noFastContextMessages = [];
for await (const message of promptMessages({ parts: [{ type: "text", text: "inspect" }] }, { NOVA_FAST_CONTEXT: "0" })) {
  noFastContextMessages.push(message);
}
assert.doesNotMatch(noFastContextMessages[0].message.content[0].text, /first repository tool call MUST/);
assert.deepEqual(noFastContextMessages[0].message.content[1], { type: "text", text: "inspect" });

const stream = { messageId: "message", blocks: new Map() };
assert.equal(streamEventItem({
  event: { type: "message_start", message: { id: "message-1" } },
}, stream), null);
assert.equal(streamEventItem({
  event: { type: "content_block_start", index: 2, content_block: { type: "text", text: "" } },
}, stream), null);
assert.deepEqual(streamEventItem({
  event: { type: "content_block_delta", index: 2, delta: { type: "text_delta", text: "完整回答" } },
}, stream), { id: "message-1-2", type: "agent_message", text: "完整回答" });

const finalItems = assistantItems({
  message: {
    // Final snapshot IDs and indexes may all differ from the stream events.
    id: "final-message-id",
    content: [
      { id: "final-text-block", type: "text", text: "完整回答" },
      { id: "final-thinking-block", type: "thinking", thinking: "完整思考" },
      { id: "tool-1", type: "tool_use", name: "Grep", input: { pattern: "榜单" } },
    ],
  },
}, stream);
assert.deepEqual(finalItems, [
  { id: "message-1-2", type: "agent_message", text: "完整回答" },
  { id: "final-thinking-block", type: "reasoning", text: "完整思考" },
  { id: "tool-1", type: "mcp_tool_call", server: "CodeBuddy", tool: "Grep", arguments: { pattern: "榜单" }, status: "in_progress" },
]);
assert.equal(finalItems[0].id, "message-1-2", "the final snapshot must replace the partial item even when its index changed");

const trailingDeltaStream = {
  messageId: "message-2",
  blocks: new Map([[0, { type: "text", text: "完整回答？" }]]),
  finalizedBlocks: [],
};
assert.deepEqual(assistantItems({
  message: {
    id: "final-trailing-delta",
    content: [{ id: "trailing-punctuation", type: "text", text: "？" }],
  },
}, trailingDeltaStream), [], "a final snapshot containing only the last delta must not duplicate punctuation");

const splitSnapshotStream = {
  messageId: "message-3",
  blocks: new Map([[0, { type: "text", text: "完整回答。" }]]),
  finalizedBlocks: [],
};
assert.deepEqual(assistantItems({
  message: { id: "main-snapshot", content: [{ type: "text", text: "完整回答。" }] },
}, splitSnapshotStream), [{ id: "message-3-0", type: "agent_message", text: "完整回答。" }]);
streamEventItem({ event: { type: "message_start", message: { id: "message-4" } } }, splitSnapshotStream);
assert.deepEqual(assistantItems({
  message: { id: "punctuation-snapshot", content: [{ id: "punctuation", type: "text", text: "。" }] },
}, splitSnapshotStream), [], "a later punctuation-only assistant snapshot must not create a standalone item");
assert.deepEqual(assistantItems({
  message: { id: "real-short-reply", content: [{ type: "text", text: "好" }] },
}, splitSnapshotStream), [{ id: "real-short-reply-0", type: "agent_message", text: "好" }], "non-punctuation replies must remain visible");

const toolStream = { messageId: "message", blocks: new Map(), tools: new Map() };
const [pendingTool] = assistantItems({
  message: { id: "assistant", content: [{ type: "tool_use", id: "tool-result-id", name: "fast_context", input: { query: "roblox" } }] },
}, toolStream);
assert.equal(pendingTool.status, "in_progress");
assert.deepEqual(toolResultItems({
  message: { content: [{ type: "tool_result", tool_use_id: "tool-result-id", content: "found", is_error: false }] },
}, toolStream), [{
  ...pendingTool,
  status: "completed",
  result: { content: [{ type: "text", text: "found" }] },
}]);
assert.equal(toolStream.tools.size, 0);
assistantItems({
  message: { id: "assistant-2", content: [{ type: "tool_use", id: "tool-without-result", name: "Grep", input: {} }] },
}, toolStream);
assert.equal(completePendingTools(toolStream)[0].status, "completed");
assert.equal(assistantText({
  message: { content: [{ type: "thinking", thinking: "ignore" }, { type: "text", text: "修复标题" }] },
}), "修复标题");

const disabledServers = novaToolsMcpServers({ cwd: "D:/repo", mode: "build" }, { NOVA_FAST_CONTEXT: "0" });
assert.equal(disabledServers, undefined);
let sdkServerOptions;
const sdkServers = novaToolsMcpServers(
  { cwd: "D:/repo", mode: "plan" },
  { NOVA_FAST_CONTEXT: "1" },
  (options) => {
    sdkServerOptions = options;
    return { type: "sdk", name: options.name, instance: {} };
  },
);
assert.deepEqual(sdkServers["nova-tools"], { type: "sdk", name: "nova-tools", instance: {} });
assert.equal(sdkServerOptions.name, "nova-tools");
assert.deepEqual(sdkServerOptions.tools.map((item) => item.name), ["fast_context", "find_symbols"]);
