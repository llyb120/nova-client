import assert from "node:assert/strict";

process.env.NOVA_CODEBUDDY_BRIDGE_TEST = "1";
const { promptMessages, resolveCodeBuddyCliPath } = await import("./codebuddy-bridge.mjs");

const npmShim = "C:\\Users\\test\\AppData\\Roaming\\npm\\codebuddy.cmd";
const npmCli = "C:\\Users\\test\\AppData\\Roaming\\npm\\node_modules\\@tencent-ai\\codebuddy-code\\bin\\codebuddy";
assert.equal(resolveCodeBuddyCliPath(npmShim, (path) => path === npmCli), npmCli);
assert.equal(resolveCodeBuddyCliPath(npmShim, () => false), npmShim);
assert.equal(resolveCodeBuddyCliPath("C:\\codebuddy\\codebuddy.exe", () => true), "C:\\codebuddy\\codebuddy.exe");

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
