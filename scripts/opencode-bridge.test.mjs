import assert from "node:assert/strict";

process.env.NOVA_OPENCODE_BRIDGE_TEST = "1";
const { automaticPermissionReply, createPromptTracker, eventProperties, promptEventState, sessionIsIdle, startPrompt, steerPrompt, todoPart, todoPlan } = await import("./opencode-bridge.mjs");

assert.equal(automaticPermissionReply("build"), "always");
assert.equal(automaticPermissionReply("plan"), undefined);

assert.deepEqual(promptEventState({ type: "session.idle", properties: { sessionID: "session-1" } }, "session-1", false), {
  started: false,
  done: false,
});
assert.deepEqual(promptEventState({
  type: "session.status",
  data: { sessionID: "session-1", status: { type: "busy" } },
}, "session-1", false), { started: true, done: false });
assert.deepEqual(promptEventState({
  type: "session.status",
  properties: { sessionID: "session-1", status: { type: "idle" } },
}, "session-1", true), { started: true, done: true });

let finishSteer;
const promptErrors = [];
const tracker = createPromptTracker((error) => promptErrors.push(error));
tracker.start(new Promise((resolve) => {
  finishSteer = resolve;
}));
let trackerSettled = false;
const trackerWait = tracker.wait().then(() => {
  trackerSettled = true;
});
await Promise.resolve();
assert.equal(trackerSettled, false);
finishSteer({});
await trackerWait;
assert.equal(trackerSettled, true);
assert.deepEqual(promptErrors, []);

assert.equal(await sessionIsIdle({
  session: { status: async () => ({ data: { "session-1": { type: "busy" } } }) },
}, "session-1"), false);
assert.equal(await sessionIsIdle({
  session: { status: async () => ({ data: {} }) },
}, "session-1"), true);

assert.deepEqual(eventProperties({ data: { sessionID: "session-1" } }), { sessionID: "session-1" });
assert.deepEqual(todoPlan([
  { content: " Connect todos ", status: "in_progress", priority: "high" },
  { content: "", status: "pending", priority: "low" },
]), [{ content: "Connect todos", status: "in_progress" }]);

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

let steerArgs;
await steerPrompt({
  v2: {
    session: {
      prompt: async (args) => {
        steerArgs = args;
        return {};
      },
    },
  },
}, "session-1", [
  { type: "text", text: "先定位根因" },
  { type: "text", text: "不要重构" },
  { type: "file", filename: "trace.png", url: "data:image/png;base64,abc", mime: "image/png" },
]);
assert.deepEqual(steerArgs, {
  sessionID: "session-1",
  prompt: {
    text: "先定位根因\n不要重构",
    files: [{ uri: "data:image/png;base64,abc", name: "trace.png" }],
  },
  delivery: "steer",
});

steerArgs = undefined;
await startPrompt({
  v2: {
    session: {
      prompt: async (args) => {
        steerArgs = args;
        return {};
      },
    },
  },
}, "session-1", {
  action: "prompt",
  delivery: "steer",
  parts: [{ type: "text", text: "改为只修复测试" }],
});
assert.deepEqual(steerArgs, {
  sessionID: "session-1",
  prompt: { text: "改为只修复测试" },
  delivery: "steer",
});
