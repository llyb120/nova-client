import assert from "node:assert/strict";

process.env.NOVA_CODEBUDDY_BRIDGE_TEST = "1";
const { assistantItems, permissionModeFor, promptMessages, resolveCodeBuddyCliPath } = await import("./codebuddy-bridge.mjs");

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

assert.deepEqual(assistantItems({
  uuid: "turn-1",
  message: {
    id: "message-1",
    content: [
      { type: "text", text: "完整回答" },
      { type: "thinking", thinking: "完整思考" },
    ],
  },
}), [
  { id: "message-1-0", type: "agent_message", text: "完整回答" },
  { id: "message-1-1", type: "reasoning", text: "完整思考" },
]);
