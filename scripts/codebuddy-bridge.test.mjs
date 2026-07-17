import assert from "node:assert/strict";

process.env.NOVA_CODEBUDDY_BRIDGE_TEST = "1";
const { promptMessages } = await import("./codebuddy-bridge.mjs");

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
