import assert from "node:assert/strict";

process.env.NOVA_CURSOR_BRIDGE_TEST = "1";
const { completePendingTools, createMessageState, mapMessage, modelSelection, promptMessage } = await import("./cursor-bridge.mjs");
const state = createMessageState();

assert.equal(mapMessage({ type: "assistant", run_id: "run", message: { content: [{ type: "text", text: "Hello" }] } }, state)[0].text, "Hello");
assert.equal(mapMessage({ type: "assistant", run_id: "run", message: { content: [{ type: "text", text: " world." }] } }, state)[0].text, "Hello world.");
assert.equal(mapMessage({ type: "thinking", run_id: "run", text: "Think" }, state)[0].text, "Think");
assert.equal(mapMessage({ type: "thinking", run_id: "run", text: "ing" }, state)[0].text, "Thinking");
assert.equal(mapMessage({ type: "thinking", run_id: "run", text: "" }, state).length, 0);
assert.equal(mapMessage({ type: "tool_call", call_id: "tool", name: "glob", status: "completed", result: {} }, state)[0].status, "completed");
assert.equal(mapMessage({ type: "tool_call", call_id: "tool", name: "glob", status: "running" }, state).length, 0);
const running = mapMessage({ type: "tool_call", call_id: "pending", name: "grep", status: "running" }, state)[0];
const afterTool = mapMessage({ type: "assistant", run_id: "run", message: { content: [{ type: "text", text: "After tool" }] } }, state)[0];
assert.notEqual(afterTool.id, "run-assistant-1");
assert.equal(running.status, "in_progress");
assert.equal(completePendingTools(state)[0].status, "completed");
const contentState = createMessageState();
const contentItems = mapMessage({ type: "assistant", run_id: "ordered", message: { content: [
  { type: "text", text: "Before" },
  { type: "tool_use", id: "embedded", name: "web_search", input: { query: "SDK auth" } },
  { type: "text", text: "After" },
] } }, contentState);
assert.deepEqual(contentItems.map((item) => item.type), ["agent_message", "mcp_tool_call", "agent_message"]);
assert.equal(contentItems[1].status, "in_progress");
assert.notEqual(contentItems[0].id, contentItems[2].id);
const embeddedDone = mapMessage({ type: "tool_call", call_id: "embedded", name: "web_search", status: "completed", result: { answer: "done" } }, contentState)[0];
assert.equal(embeddedDone.id, "embedded");
assert.equal(embeddedDone.status, "completed");
assert.deepEqual(embeddedDone.arguments, { query: "SDK auth" });
assert.deepEqual(modelSelection("cursor-grok-4.5-high-fast"), { id: "grok-4.5", params: [{ id: "effort", value: "high" }, { id: "fast", value: "true" }] });
assert.deepEqual(modelSelection("composer-2.5-fast"), { id: "composer-2.5", params: [{ id: "fast", value: "true" }] });
assert.deepEqual(modelSelection("gpt-5.6-sol"), { id: "gpt-5.6-sol" });
assert.deepEqual(await promptMessage([{ type: "text", text: "look" }, { type: "image_data", mime: "image/png", data: "base64" }]), { text: "look", images: [{ data: "base64", mimeType: "image/png" }] });
