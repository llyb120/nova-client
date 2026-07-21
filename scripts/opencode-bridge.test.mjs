import assert from "node:assert/strict";

process.env.NOVA_OPENCODE_BRIDGE_TEST = "1";
const { applyV2Event, automaticPermissionReply, createPromptTracker, ensureSession, eventProperties, listModels, promptEventState, sessionIsIdle, startPrompt, steerPrompt, todoPart, todoPlan } = await import("./opencode-bridge.mjs");

assert.equal(automaticPermissionReply("build"), "always");
assert.equal(automaticPermissionReply("plan"), undefined);

assert.deepEqual(promptEventState({ type: "session.idle", data: { sessionID: "session-1" } }, "session-1", false), {
  started: false,
  done: false,
});
assert.deepEqual(promptEventState({
  type: "session.next.step.started",
  data: { sessionID: "session-1" },
}, "session-1", false), { started: true, done: false });
assert.deepEqual(promptEventState({
  type: "session.status",
  data: { sessionID: "session-1", status: { type: "idle" } },
}, "session-1", true), { started: true, done: true });

let finishSteer;
const promptErrors = [];
const tracker = createPromptTracker((error) => promptErrors.push(error));
tracker.start(new Promise((resolve) => { finishSteer = resolve; }));
let trackerSettled = false;
const trackerWait = tracker.wait().then(() => { trackerSettled = true; });
await Promise.resolve();
assert.equal(trackerSettled, false);
finishSteer({});
await trackerWait;
assert.equal(trackerSettled, true);
assert.deepEqual(promptErrors, []);

assert.equal(await sessionIsIdle({
  v2: { session: { active: async () => ({ data: { data: { "session-1": { type: "running" } } } }) } },
}, "session-1"), false);
assert.equal(await sessionIsIdle({
  v2: { session: { active: async () => ({ data: { data: {} } }) } },
}, "session-1"), true);

let createSessionArgs;
assert.equal(await ensureSession({
  v2: { session: { create: async (args) => {
    createSessionArgs = args;
    return { data: { data: { id: "session-new" } } };
  } } },
}), "session-new");
assert.deepEqual(createSessionArgs, { location: { directory: process.cwd() } });

assert.deepEqual(eventProperties({ data: { sessionID: "session-1" } }), { sessionID: "session-1" });
assert.deepEqual(await listModels({
  v2: {
    provider: { list: async () => ({ data: { data: [{ id: "openai", name: "OpenAI" }] } }) },
    model: { list: async () => ({ data: { data: [{
      id: "gpt-5", providerID: "openai", name: "GPT-5", enabled: true,
      variants: [{ id: "high" }], capabilities: { tools: true, input: ["text", "image"], output: ["text"] },
    }] } }) },
  },
}), { all: [{
  id: "openai",
  name: "OpenAI",
  models: {
    "gpt-5": {
      name: "GPT-5",
      variants: ["high"],
      capabilities: { attachment: true, input: { image: true, pdf: false } },
    },
  },
}] });
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
const switches = [];
await startPrompt({
  v2: {
    session: {
      switchModel: async (args) => { switches.push(args); return {}; },
      prompt: async (args) => { promptArgs = args; return {}; },
    },
  },
}, "session-1", {
  action: "prompt",
  model: { providerID: "openai", modelID: "gpt-5" },
  variant: "high",
  parts: [{ type: "text", text: "继续检查" }],
});
assert.deepEqual(switches, [{
  sessionID: "session-1",
  model: { providerID: "openai", id: "gpt-5", variant: "high" },
}]);
assert.deepEqual(promptArgs, {
  sessionID: "session-1",
  prompt: { text: "继续检查" },
  delivery: "queue",
});

let steerArgs;
await steerPrompt({
  v2: { session: { prompt: async (args) => { steerArgs = args; return {}; } } },
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

const parts = new Map();
assert.deepEqual(applyV2Event({
  type: "session.next.text.delta",
  data: { sessionID: "session-1", assistantMessageID: "message-1", textID: "text-1", delta: "完成" },
}, parts), {
  sessionID: "session-1", messageID: "message-1", id: "text-1", type: "text", text: "完成",
});
assert.equal(applyV2Event({
  type: "session.next.text.delta",
  data: { sessionID: "session-1", assistantMessageID: "message-1", textID: "text-1", delta: "修复" },
}, parts).text, "完成修复");
applyV2Event({
  type: "session.next.tool.called",
  data: { sessionID: "session-1", assistantMessageID: "message-1", callID: "call-1", tool: "read", input: { path: "a.ts" } },
}, parts);
assert.deepEqual(applyV2Event({
  type: "session.next.tool.success",
  data: { sessionID: "session-1", assistantMessageID: "message-1", callID: "call-1", result: "ok" },
}, parts).state, { status: "completed", input: { path: "a.ts" }, output: "ok" });
