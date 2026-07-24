import assert from "node:assert/strict";

process.env.NOVA_CODEBUDDY_BRIDGE_TEST = "1";
const {
  assistantItems,
  assistantText,
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
    id: "message-1",
    content: [
      { type: "text", text: "完整回答" },
      { type: "thinking", thinking: "完整思考" },
    ],
  },
});
assert.deepEqual(finalItems, [
  { id: "message-1-0", type: "agent_message", text: "完整回答" },
  { id: "message-1-1", type: "reasoning", text: "完整思考" },
]);
assert.equal(finalItems[0].id, "message-1-0", "the final snapshot must replace the partial item");
assert.equal(assistantText({
  message: { content: [{ type: "thinking", thinking: "ignore" }, { type: "text", text: "修复标题" }] },
}), "修复标题");
