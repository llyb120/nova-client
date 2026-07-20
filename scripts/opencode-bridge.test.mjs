import assert from "node:assert/strict";

process.env.NOVA_OPENCODE_BRIDGE_TEST = "1";
const { automaticPermissionReply, promptEventState, startPrompt, todoPart } = await import("./opencode-bridge.mjs");

assert.equal(automaticPermissionReply("build"), "always");
assert.equal(automaticPermissionReply("plan"), undefined);

assert.deepEqual(promptEventState({ type: "session.idle", properties: { sessionID: "session-1" } }, "session-1", false), {
  started: false,
  done: false,
});
assert.deepEqual(promptEventState({
  type: "session.status",
  properties: { sessionID: "session-1", status: { type: "busy" } },
}, "session-1", false), { started: true, done: false });
assert.deepEqual(promptEventState({
  type: "session.status",
  properties: { sessionID: "session-1", status: { type: "idle" } },
}, "session-1", true), { started: true, done: true });

assert.deepEqual(todoPart("session-1", [{ content: "Connect todos", status: "in_progress", priority: "high" }]), {
  id: "nova-todo-session-1",
  sessionID: "session-1",
  messageID: "nova-todo-session-1",
  type: "tool",
  callID: "nova-todo-session-1",
  tool: "todowrite",
  state: {
    status: "completed",
    input: { todos: [{ content: "Connect todos", status: "in_progress", priority: "high" }] },
  },
});

let promptArgs;
await startPrompt({
  session: {
    promptAsync: async (args) => {
      promptArgs = args;
      return {};
    },
  },
}, "session-1", {
  action: "prompt",
  model: { providerID: "openai", modelID: "gpt-5" },
  variant: "high",
  parts: [{ type: "text", text: "继续检查" }],
});
assert.deepEqual(promptArgs, {
  sessionID: "session-1",
  model: { providerID: "openai", modelID: "gpt-5" },
  variant: "high",
  parts: [{ type: "text", text: "继续检查" }],
});
