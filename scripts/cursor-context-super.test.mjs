import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

process.env.NOVA_CURSOR_BRIDGE_TEST = "1";
const {
  CursorStartupTimeout,
  compactConversation,
  completePendingTools,
  compressSlimMemory,
  contextTokensFromUsage,
  cursorRunUsage,
  mergeCursorUsage,
  normalizeCursorUsageForNova,
  createCursorAgent,
  createCursorFilesystemTools,
  createMessageState,
  createSlimMemory,
  EDIT_FILES_INLINE_CHAR_LIMIT,
  decodeFilesB64,
  editFilesPathsFromArgs,
  recommendEditFilesTransport,
  cursorBatchToolPolicy,
  cursorCavemanPolicy,
  cursorPromptPrefix,
  cursorModelOptions,
  cursorShellProgram,
  cursorTodoPlan,
  ensureGlobalTaskDenyHooks,
  extractTurnConclusion,
  formatCompletedTurn,
  formatInterruptedTurn,
  formatSlimMemory,
  ingestCompactHistory,
  isEditFilesTool,
  isNovaDenyTaskHook,
  isRetryableCursorError,
  shouldSilentRetryCursorTurn,
  isSlimMemoryEmpty,
  mapDelta,
  mapMessage,
  mergeNovaTaskDenyHooks,
  messageWithRecoveryContext,
  messageWithSlimMemory,
  messageWithToolPolicy,
  modelSelection,
  novaDenyTaskHookCommand,
  pendingTurnContext,
  promptMessage,
  recordSlimTurn,
  recoverTimedOutAgent,
  sendPromptWithRecovery,
  threadMemoryKey,
  withTimeout,
  agentFingerprint,
  createAgentPrewarm,
} = await import("./cursor-context-super.mjs");
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
const deltaState = createMessageState();
assert.equal(mapDelta({ type: "thinking-delta", text: "Think" }, deltaState, "delta").text, "Think");
assert.equal(mapDelta({ type: "thinking-delta", text: "ing" }, deltaState, "delta").text, "Thinking");
assert.equal(mapMessage({ type: "thinking", run_id: "delta", text: "Thinking" }, deltaState).length, 0);
assert.equal(mapDelta({ type: "text-delta", text: "Hello" }, deltaState, "delta").text, "Hello");
assert.equal(mapMessage({ type: "assistant", run_id: "delta", message: { content: [{ type: "text", text: "Hello" }] } }, deltaState).length, 0);
const deltaTool = mapDelta({ type: "tool-call-started", callId: "read", toolCall: { type: "read", args: { path: "README.md" } } }, deltaState, "delta");
assert.equal(deltaTool.status, "in_progress");
assert.deepEqual(deltaTool.arguments, { path: "README.md" });
assert.equal(mapDelta({ type: "tool-call-completed", callId: "read", toolCall: { type: "read", result: { status: "success", value: "ok" } } }, deltaState, "delta").status, "completed");
const mcpTool = mapDelta({
  type: "tool-call-started",
  callId: "mcp-read",
  toolCall: {
    type: "mcp",
    args: {
      args: { paths: [{ path: "src/a.ts", offset: 10, limit: 20 }] },
      providerIdentifier: "custom-user-tools",
      toolName: "read_files",
    },
  },
}, deltaState, "delta");
assert.equal(mcpTool.tool, "read_files");
assert.deepEqual(mcpTool.arguments, { paths: [{ path: "src/a.ts", offset: 10, limit: 20 }] });
const completedMcpTool = mapDelta({
  type: "tool-call-completed",
  callId: "mcp-read",
  toolCall: { type: "mcp", result: { status: "success", value: { content: [{ type: "text", text: "ok" }] } } },
}, deltaState, "delta");
assert.equal(completedMcpTool.tool, "read_files");
assert.deepEqual(completedMcpTool.result, { content: [{ type: "text", text: "ok" }] });
assert.deepEqual(cursorTodoPlan({ type: "updateTodos", args: { todos: [
  { content: " Inspect repository ", status: "completed" },
  { content: "Implement fix", status: "inProgress" },
  { content: " ", status: "pending" },
] } }), [
  { content: "Inspect repository", status: "completed" },
  { content: "Implement fix", status: "in_progress" },
]);
assert.deepEqual(cursorTodoPlan({ type: "updateTodos", args: { todos: [] }, result: { status: "success", value: { todos: [
  { content: "Verify", status: "cancelled" },
] } } }), [{ content: "Verify", status: "cancelled" }]);
assert.equal(cursorTodoPlan({ type: "read", args: {} }), null);
assert.equal(cursorShellProgram("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe", {
  NOVA_SHELL_SHIM_POWERSHELL: "C:\\Nova\\shim\\powershell.exe",
}), process.platform === "win32"
  ? "C:\\Nova\\shim\\powershell.exe"
  : "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe");
assert.equal(cursorShellProgram("tool.exe", { NOVA_SHELL_SHIM_POWERSHELL: "shim.exe" }), "tool.exe");
{
  const root = await mkdtemp(join(tmpdir(), "nova-cursor-shell-env-"));
  const psDir = join(root, "System32", "WindowsPowerShell", "v1.0");
  await mkdir(psDir, { recursive: true });
  const ps = join(psDir, "powershell.exe");
  await writeFile(ps, "ps");
  const missingPwsh = join(root, "missing", "pwsh.exe");
  const realPwsh = join(root, "real-pwsh.exe");
  await writeFile(realPwsh, "pwsh");
  assert.equal(
    cursorShellProgram(missingPwsh, { SystemRoot: root, PATH: "" }),
    process.platform === "win32" ? ps : missingPwsh,
  );
  assert.equal(
    cursorShellProgram(missingPwsh, {
      NOVA_SHELL_SHIM_PWSH_REAL: realPwsh,
      SystemRoot: root,
      PATH: "",
    }),
    process.platform === "win32" ? realPwsh : missingPwsh,
  );
  assert.equal(
    cursorShellProgram(missingPwsh, {
      NOVA_SHELL_SHIM_PWSH: "C:\\Nova\\shim\\pwsh.exe",
      NOVA_SHELL_SHIM_PWSH_REAL: realPwsh,
      SystemRoot: root,
      PATH: "",
    }),
    process.platform === "win32" ? "C:\\Nova\\shim\\pwsh.exe" : missingPwsh,
  );
  await rm(root, { recursive: true, force: true });
}
assert.deepEqual(modelSelection("cursor-grok-4.5-high-fast"), { id: "grok-4.5", params: [{ id: "effort", value: "high" }, { id: "fast", value: "true" }] });
assert.deepEqual(modelSelection("grok-4.5-high-false"), { id: "grok-4.5", params: [{ id: "effort", value: "high" }, { id: "fast", value: "false" }] });
assert.deepEqual(modelSelection("composer-2.5-fast"), { id: "composer-2.5", params: [{ id: "fast", value: "true" }] });
assert.deepEqual(modelSelection("gpt-5.6-sol"), { id: "gpt-5.6-sol" });
assert.deepEqual(modelSelection("grok-4.5::effort=high&fast=false"), { id: "grok-4.5", params: [{ id: "effort", value: "high" }, { id: "fast", value: "false" }] });
assert.equal(
  agentFingerprint({ model: "grok-4.5-high-fast", cwd: "/tmp/a", mode: "agent" }),
  agentFingerprint({ model: "grok-4.5-high-fast", cwd: "/tmp/a" }),
);
assert.notEqual(
  agentFingerprint({ model: "grok-4.5-high-fast", cwd: "/tmp/a", mode: "plan" }),
  agentFingerprint({ model: "grok-4.5-high-fast", cwd: "/tmp/a", mode: "agent" }),
);
{
  let createCount = 0;
  const created = [];
  const deferred = [];
  const sdk = {
    create: async (options) => {
      createCount += 1;
      let resolve;
      const gate = new Promise((r) => { resolve = r; });
      deferred.push(resolve);
      await gate;
      const agent = {
        agentId: `prewarm-${createCount}`,
        options,
        closed: false,
        close() { this.closed = true; },
      };
      created.push(agent);
      return agent;
    },
  };
  const prewarm = createAgentPrewarm(sdk, { enabled: true });
  const req = { model: "composer-2.5-fast", cwd: "/tmp/ws", mode: "agent" };
  async function waitReady() {
    for (let i = 0; i < 10; i++) {
      if (prewarm.snapshot?.ready) return;
      await Promise.resolve();
    }
    assert.equal(prewarm.snapshot?.ready, true);
  }

  const firstPromise = prewarm.acquire(req);
  assert.equal(createCount, 1);
  deferred.shift()();
  const first = await firstPromise;
  assert.equal(first.source, "live");
  assert.equal(first.agent.agentId, "prewarm-1");
  // Consume/live path immediately starts the refill create.
  assert.equal(createCount, 2);
  assert.equal(prewarm.snapshot?.pending, true);

  const secondPromise = prewarm.acquire(req);
  assert.equal(createCount, 2);
  deferred.shift()();
  const second = await secondPromise;
  assert.equal(second.source, "prewarm_wait");
  assert.equal(second.agent.agentId, "prewarm-2");
  // Refill again as soon as the warm slot is consumed.
  assert.equal(createCount, 3);
  deferred.shift()();
  await waitReady();

  const third = await prewarm.acquire(req);
  assert.equal(third.source, "prewarm");
  assert.equal(third.agent.agentId, "prewarm-3");
  assert.equal(createCount, 4);

  const mismatchedPromise = prewarm.acquire({ model: "gpt-5.6-sol", cwd: "/tmp/ws", mode: "agent" });
  // Mismatch discards the pending refill and starts a live create for the new fingerprint.
  assert.equal(createCount, 5);
  deferred.shift()();
  for (let i = 0; i < 10 && !created[3]?.closed; i++) await Promise.resolve();
  assert.equal(created[3].closed, true);
  deferred.shift()();
  const fourth = await mismatchedPromise;
  assert.equal(fourth.source, "live");
  assert.equal(fourth.agent.agentId, "prewarm-5");
  assert.equal(createCount, 6);
  deferred.shift()();
  await waitReady();
  prewarm.close();
  assert.equal(created[5].closed, true);
}
assert.deepEqual(cursorModelOptions([
  { id: "auto", displayName: "Auto" },
  { id: "default", displayName: "Auto" },
  { id: "grok-4.5", displayName: "Cursor Grok 4.5", parameters: [
    { id: "effort", displayName: "Effort", values: [{ value: "high", displayName: "High" }] },
    { id: "fast", displayName: "Fast", values: [{ value: "false" }, { value: "true", displayName: "Fast" }] },
  ], variants: [
    { displayName: "Cursor Grok 4.5", params: [{ id: "effort", value: "high" }, { id: "fast", value: "false" }] },
    { displayName: "Cursor Grok 4.5", params: [{ id: "effort", value: "high" }, { id: "fast", value: "true" }] },
  ] },
]), [
  { value: "", name: "Auto（Cursor 默认）" },
  { value: "grok-4.5::effort=high&fast=false", name: "Cursor Grok 4.5 High", description: undefined },
  { value: "grok-4.5::effort=high&fast=true", name: "Cursor Grok 4.5 High Fast", description: undefined },
]);
assert.deepEqual(await promptMessage([{ type: "text", text: "look" }, { type: "image_data", mime: "image/png", data: "base64" }]), { text: "look", images: [{ data: "base64", mimeType: "image/png" }] });
assert.equal(await promptMessage([{ type: "text", text: "inspect" }, { type: "local_image", path: "C:/Users/1/Desktop/1.xlsx" }]), "inspect\n\nAttached file: C:/Users/1/Desktop/1.xlsx");
assert.equal(threadMemoryKey("thread-a"), "nova-thread-thread-a");
assert.notEqual(threadMemoryKey("thread-a"), threadMemoryKey("thread-b"));
assert.match(threadMemoryKey("../unsafe/thread"), /^nova-thread-[a-f0-9]{64}$/);
assert.equal(threadMemoryKey(undefined), undefined);
assert.equal(isRetryableCursorError({ isRetryable: true, code: "unavailable" }), true);
assert.equal(isRetryableCursorError(new TypeError("Failed to connect to API key exchange endpoint: fetch failed")), true);
assert.equal(isRetryableCursorError(new Error("Invalid API key")), false);
assert.equal(isRetryableCursorError({
  rawMessage: "read ECONNRESET",
  code: 10,
  cause: Object.assign(new Error("read ECONNRESET"), { code: "ECONNRESET" }),
}), true);
assert.equal(isRetryableCursorError({ code: "aborted", message: "stream aborted" }), true);
assert.equal(shouldSilentRetryCursorTurn(
  Object.assign(new Error("read ECONNRESET"), { code: "ECONNRESET" }),
  { producedOutput: false, attempt: 0 },
), true);
assert.equal(shouldSilentRetryCursorTurn(
  Object.assign(new Error("read ECONNRESET"), { code: "ECONNRESET" }),
  { producedOutput: true, attempt: 0 },
), true);
assert.equal(shouldSilentRetryCursorTurn(
  new CursorStartupTimeout("stream startup"),
  { producedOutput: false, attempt: 0 },
), true);
assert.equal(shouldSilentRetryCursorTurn(
  new Error("Invalid API key"),
  { producedOutput: false, attempt: 0 },
), false);
let createAttempts = 0;
const createdAgent = { agentId: "created" };
assert.equal(await createCursorAgent({}, {
  create: async () => {
    createAttempts += 1;
    if (createAttempts < 3) throw Object.assign(new Error("fetch failed"), { isRetryable: true });
    return createdAgent;
  },
}, [0, 0]), createdAgent);
assert.equal(createAttempts, 3);

const recoveryCalls = [];
let sendAttempts = 0;
const recoveredRun = { id: "new-run" };
const recoverableAgent = {
  agentId: "agent-1",
  send: async () => {
    sendAttempts += 1;
    if (sendAttempts === 1) throw new Error("already has active run");
    return recoveredRun;
  },
  close: () => recoveryCalls.push("close"),
};
const recoverySdk = {
  listRuns: async () => ({ items: [{ id: "stale", status: "running" }, { id: "done", status: "completed" }] }),
  cancelRun: async (id) => recoveryCalls.push(`cancel:${id}`),
  resume: async () => {
    recoveryCalls.push("resume");
    return recoverableAgent;
  },
};
const timingPhases = [];
const recovered = await sendPromptWithRecovery(
  recoverableAgent,
  { cwd: "." },
  "continue",
  {},
  recoverySdk,
  (phase) => timingPhases.push(phase),
);
assert.equal(recovered.agent, recoverableAgent);
assert.equal(recovered.run, recoveredRun);
assert.deepEqual(recoveryCalls, ["cancel:stale"]);
assert.deepEqual(timingPhases, ["send_active_run", "active_run_cleanup", "send_retry"]);

let fallbackAttempts = 0;
const resumedRun = { id: "resumed-run" };
const fallbackAgent = {
  agentId: "agent-2",
  send: async () => {
    fallbackAttempts += 1;
    throw new Error("already has active run");
  },
  close: () => recoveryCalls.push("fallback-close"),
};
const resumedAgent = { send: async () => resumedRun };
const fallbackSdk = {
  listRuns: async () => ({ items: [{ id: "queued", status: "queued" }] }),
  cancelRun: async (id) => recoveryCalls.push(`cancel:${id}`),
  resume: async () => resumedAgent,
};
const fallback = await sendPromptWithRecovery(fallbackAgent, { cwd: "." }, "continue", {}, fallbackSdk, () => {});
assert.equal(fallback.agent, resumedAgent);
assert.equal(fallback.run, resumedRun);
assert.equal(fallbackAttempts, 2);

await assert.rejects(
  withTimeout(new Promise(() => {}), 10, "test"),
  (error) => error instanceof CursorStartupTimeout && error.message.includes("test"),
);
assert.equal(await withTimeout(Promise.resolve("ready"), 10, "test"), "ready");

const timeoutRecoveryCalls = [];
const replacementAgent = { agentId: "replacement" };
const timedOutAgent = {
  agentId: "timed-out-agent",
  close: () => timeoutRecoveryCalls.push("close"),
};
const timeoutRecoverySdk = {
  listRuns: async () => ({ items: [{ id: "active", status: "running" }, { id: "finished", status: "completed" }] }),
  cancelRun: async (id) => timeoutRecoveryCalls.push(`cancel:${id}`),
  resume: async () => assert.fail("slim recovery must create a fresh agent"),
  create: async () => {
    timeoutRecoveryCalls.push("create");
    return replacementAgent;
  },
};
const recoveredAfterTimeout = await recoverTimedOutAgent(
  timedOutAgent,
  { cancel: async () => timeoutRecoveryCalls.push("cancel-current") },
  { cwd: ".", model: "grok-4.5-high-fast" },
  timeoutRecoverySdk,
  100,
);
assert.equal(recoveredAfterTimeout.agent, replacementAgent);
assert.equal(recoveredAfterTimeout.replaced, true);
assert.deepEqual(timeoutRecoveryCalls, ["cancel-current", "cancel:active", "close", "create"]);

const conversation = [
  { type: "agentConversationTurn", turn: { userMessage: { text: "Build a restaurant" }, steps: [
    { type: "toolCall", message: { type: "write" } },
    { type: "assistantMessage", message: { text: "Created the first version." } },
  ] } },
  { type: "agentConversationTurn", turn: { userMessage: { text: "Make it bright" }, steps: [
    { type: "assistantMessage", message: { text: "Changed the lighting." } },
  ] } },
];
assert.equal(compactConversation(conversation), [
  "User: Build a restaurant",
  "Assistant: Created the first version.",
  "User: Make it bright",
  "Assistant: Changed the lighting.",
].join("\n\n"));
assert.ok(compactConversation(conversation, 40).endsWith("Assistant: Changed the lighting."));
const recoveredMessage = messageWithRecoveryContext(
  { text: "Add animation", images: [{ data: "image", mimeType: "image/png" }] },
  compactConversation(conversation),
);
assert.match(recoveredMessage.text, /Created the first version/);
assert.match(recoveredMessage.text, /Current request:\nAdd animation$/);
assert.deepEqual(recoveredMessage.images, [{ data: "image", mimeType: "image/png" }]);

const freshCalls = [];
const freshAgent = { agentId: "fresh-agent" };
const finishedRun = {
  id: "finished-run",
  status: "finished",
  createdAt: 2,
  conversation: async () => conversation,
};
const freshSdk = {
  listRuns: async () => ({ items: [finishedRun, { id: "stuck", status: "running" }] }),
  cancelRun: async (id) => freshCalls.push(`cancel:${id}`),
  resume: async () => assert.fail("a poisoned agent must not be resumed"),
  create: async () => {
    freshCalls.push("create");
    return freshAgent;
  },
};
const freshRecovery = await recoverTimedOutAgent(
  { agentId: "poisoned", close: () => freshCalls.push("close") },
  { cancel: async () => freshCalls.push("cancel-current") },
  { cwd: ".", model: "grok-4.5-high-fast" },
  freshSdk,
  100,
  true,
);
assert.equal(freshRecovery.agent, freshAgent);
assert.equal(freshRecovery.replaced, true);
assert.equal(freshRecovery.history, "");
assert.deepEqual(freshCalls, ["cancel-current", "cancel:stuck", "close", "create"]);

const slim = createSlimMemory();
assert.equal(isSlimMemoryEmpty(slim), true);
recordSlimTurn(slim, "Build a restaurant", "Created the first version.");
recordSlimTurn(slim, "Make it bright", "Changed the lighting.");
assert.deepEqual(slim.turns, [
  { userPrompt: "Build a restaurant", conclusion: "Created the first version." },
  { userPrompt: "Make it bright", conclusion: "Changed the lighting." },
]);
assert.match(formatSlimMemory(slim), /Recent turns/);
const slimMessage = messageWithSlimMemory(
  { text: "Add animation", images: [{ data: "image", mimeType: "image/png" }] },
  slim,
);
assert.match(slimMessage.text, /Changed the lighting/);
assert.match(slimMessage.text, /Current request:\nAdd animation$/);
assert.deepEqual(slimMessage.images, [{ data: "image", mimeType: "image/png" }]);
assert.equal(messageWithSlimMemory("only current", createSlimMemory()), "only current");

const interruptedState = createMessageState();
mapMessage({ type: "thinking", run_id: "interrupted", text: "Need to inspect the file first." }, interruptedState);
mapMessage({ type: "assistant", run_id: "interrupted", message: { content: [{ type: "text", text: "I inspected the file." }] } }, interruptedState);
mapMessage({
  type: "tool_call",
  run_id: "interrupted",
  id: "read-call",
  name: "read_file",
  args: { path: "src/app.ts" },
  result: { content: "const answer = 42;" },
}, interruptedState);
const interruptedContext = formatInterruptedTurn("Fix the unfinished change", interruptedState);
assert.match(interruptedContext, /Fix the unfinished change/);
assert.match(interruptedContext, /Assistant reasoning:\nNeed to inspect the file first/);
assert.match(interruptedContext, /I inspected the file/);
assert.match(interruptedContext, /read_file/);
assert.match(interruptedContext, /src\/app\.ts/);
assert.match(interruptedContext, /answer = 42/);
const completedContext = formatCompletedTurn("Fix the unfinished change", interruptedState);
assert.match(completedContext, /Fix the unfinished change/);
assert.match(completedContext, /I inspected the file/);
assert.match(completedContext, /read_file/);
assert.doesNotMatch(completedContext, /Assistant reasoning/);
assert.doesNotMatch(completedContext, /Need to inspect the file first/);
const completedMemory = createSlimMemory();
completedMemory.fullTurns = [completedContext];
assert.doesNotMatch(formatSlimMemory(completedMemory), /Assistant reasoning/);
const interruptedMemory = createSlimMemory();
interruptedMemory.pendingTurn = interruptedContext;
assert.equal(isSlimMemoryEmpty(interruptedMemory), false);
assert.match(messageWithSlimMemory("Continue", interruptedMemory), /complete working context/);
assert.match(messageWithSlimMemory("Continue", interruptedMemory), /Assistant reasoning/);
const failedBeforeSdkStart = pendingTurnContext("", "Inspect the repository");
assert.match(failedBeforeSdkStart, /^User:\nInspect the repository$/);
const failedAgain = pendingTurnContext(failedBeforeSdkStart, "go on");
assert.match(failedAgain, /^User:\nInspect the repository/);
assert.match(failedAgain, /User:\ngo on$/);
const failedWithTrace = pendingTurnContext(failedBeforeSdkStart, "go on", interruptedState);
assert.match(failedWithTrace, /Inspect the repository/);
assert.match(failedWithTrace, /Assistant reasoning/);

const seeded = createSlimMemory();
ingestCompactHistory(seeded, compactConversation(conversation));
assert.deepEqual(seeded.turns, [
  { userPrompt: "Build a restaurant", conclusion: "Created the first version." },
  { userPrompt: "Make it bright", conclusion: "Changed the lighting." },
]);

const compressible = createSlimMemory();
for (let index = 1; index <= 10; index += 1) {
  recordSlimTurn(compressible, `user prompt ${index}`, `conclusion ${index}`);
}
compressible.contextStage = "slim";
assert.equal(
  await compressSlimMemory(compressible, async () => assert.fail("stage one must not summarize"), {
    currentTokens: 0,
    maxTokens: 750,
    maxChars: 100_000,
  }),
  false,
);
recordSlimTurn(compressible, "latest user prompt must remain exact", "latest conclusion");
let summaryInput = "";
assert.equal(await compressSlimMemory(compressible, async (input) => {
  summaryInput = input;
  return "Summary of the first ten turns.";
}, { currentTokens: 750, maxTokens: 750 }), true);
assert.match(summaryInput, /user prompt 1/);
assert.match(summaryInput, /user prompt 10/);
assert.doesNotMatch(summaryInput, /latest user prompt/);
assert.equal(compressible.summary, "Summary of the first ten turns.");
assert.deepEqual(compressible.turns, [{
  userPrompt: "latest user prompt must remain exact",
  conclusion: "latest conclusion",
}]);
assert.equal(contextTokensFromUsage({ totalTokens: 900, outputTokens: 100, inputTokens: 800 }), 800);
assert.equal(contextTokensFromUsage({ input_tokens: 700, output_tokens: 50 }), 700);
assert.equal(contextTokensFromUsage({ inputTokens: 700, cacheWriteTokens: 100 }), 800);
const firstModelTurnUsage = {
  inputTokens: 100,
  outputTokens: 20,
  cacheReadTokens: 200,
  cacheWriteTokens: 40,
};
const lastModelTurnUsage = {
  inputTokens: 120,
  outputTokens: 30,
  cacheReadTokens: 240,
  cacheWriteTokens: 0,
};
const completeRunUsage = {
  inputTokens: 220,
  outputTokens: 50,
  cacheReadTokens: 440,
  cacheWriteTokens: 40,
  totalTokens: 750,
};
assert.deepEqual(mergeCursorUsage(firstModelTurnUsage, lastModelTurnUsage), completeRunUsage);
assert.equal(cursorRunUsage({ usage: completeRunUsage }, lastModelTurnUsage), completeRunUsage);
assert.equal(cursorRunUsage({}, completeRunUsage), completeRunUsage);
assert.deepEqual(normalizeCursorUsageForNova(completeRunUsage), {
  ...completeRunUsage,
  inputTokens: 0,
  outputTokens: 10,
  totalTokens: 490,
});

const conclusionState = createMessageState();
conclusionState.texts.set("run-assistant-1", "Final answer from the assistant.");
assert.equal(
  extractTurnConclusion(conclusionState, { result: "Prefer result text." }),
  "Prefer result text.",
);
assert.equal(
  extractTurnConclusion(conclusionState, {}),
  "Final answer from the assistant.",
);

const mergedHooks = mergeNovaTaskDenyHooks({
  version: 1,
  hooks: {
    afterFileEdit: [{ command: "./hooks/format.sh" }],
    preToolUse: [
      { command: "echo keep-me", matcher: "Shell" },
      { command: "node ./hooks/nova-deny-task.mjs", matcher: "Task", failClosed: false },
    ],
  },
});
assert.equal(mergedHooks.hooks.afterFileEdit[0].command, "./hooks/format.sh");
assert.equal(mergedHooks.hooks.preToolUse.find((entry) => entry.matcher === "Shell")?.command, "echo keep-me");
assert.equal(mergedHooks.hooks.preToolUse.filter(isNovaDenyTaskHook).length, 1);
assert.deepEqual(mergedHooks.hooks.preToolUse.find(isNovaDenyTaskHook), {
  command: novaDenyTaskHookCommand(),
  failClosed: true,
  matcher: "Task",
});
assert.deepEqual(mergedHooks.hooks.subagentStart, [{
  command: novaDenyTaskHookCommand(),
  failClosed: true,
}]);

const cursorDir = await mkdtemp(join(tmpdir(), "nova-cursor-hooks-"));
try {
  const ensured = await ensureGlobalTaskDenyHooks(cursorDir);
  const written = JSON.parse(await readFile(ensured.hooksPath, "utf8"));
  const script = await readFile(ensured.scriptPath, "utf8");
  assert.match(script, /permission: "deny"/);
  assert.equal(written.hooks.preToolUse[0].matcher, "Task");
  assert.equal(written.hooks.subagentStart[0].command, novaDenyTaskHookCommand());
  const again = await ensureGlobalTaskDenyHooks(cursorDir);
  assert.deepEqual(again.config.hooks.preToolUse, written.hooks.preToolUse);
} finally {
  await rm(cursorDir, { recursive: true, force: true });
}

assert.match(cursorBatchToolPolicy(), /read_files/);
assert.match(cursorBatchToolPolicy(), /edit_files/);
assert.match(cursorBatchToolPolicy(), /filesB64/);
assert.doesNotMatch(cursorBatchToolPolicy(), /filesPath/);
assert.match(cursorBatchToolPolicy(), /git grep/);
assert.doesNotMatch(cursorBatchToolPolicy({ readOnly: true }), /edit_files/);
assert.match(cursorBatchToolPolicy({ readOnly: true }), /plan\/read-only/);
assert.match(cursorCavemanPolicy(), /回复默认简洁专业/);
assert.match(cursorCavemanPolicy(), /先给结论/);
assert.doesNotMatch(cursorCavemanPolicy(), /Respond terse like smart caveman/);
assert.match(cursorPromptPrefix(), /read_files/);
assert.match(cursorPromptPrefix(), /回复默认简洁专业/);
assert.match(cursorPromptPrefix({ readOnly: true }), /plan\/read-only/);
assert.doesNotMatch(cursorPromptPrefix({ readOnly: true }), /edit_files/);
const policyMessage = messageWithToolPolicy("Add animation", { readOnly: false });
assert.match(policyMessage, /read_files/);
assert.match(policyMessage, /回复默认简洁专业/);
assert.match(policyMessage, /Add animation$/);
const slimWithPolicy = messageWithToolPolicy(messageWithSlimMemory("Continue", slim), { readOnly: false });
assert.match(slimWithPolicy, /read_files/);
assert.match(slimWithPolicy, /回复默认简洁专业/);
assert.match(slimWithPolicy, /Changed the lighting/);
assert.match(slimWithPolicy, /Current request:\nContinue$/);
assert.equal(isEditFilesTool("edit_files"), true);
assert.equal(isEditFilesTool("mcp__custom-user-tools__edit_files"), true);
assert.equal(isEditFilesTool("read_files"), false);
const editFilesState = createMessageState();
const editFilesItem = mapMessage({
  type: "tool_call",
  call_id: "edit-batch",
  name: "edit_files",
  status: "running",
  args: { files: [{ path: "a.ts", edits: [{ oldText: "a", newText: "b" }] }] },
}, editFilesState)[0];
assert.equal(editFilesItem.type, "file_change");
assert.deepEqual(editFilesItem.changes, [{ path: "a.ts", kind: "update" }]);

const batchCwd = await mkdtemp(join(tmpdir(), "nova-cursor-batch-"));
try {
  await Promise.all([
    writeFile(join(batchCwd, "a.txt"), "A\nconst quoted = \"x\";"),
    writeFile(join(batchCwd, "b.txt"), "B\npath = C:\\tmp"),
  ]);
  const agentTools = createCursorFilesystemTools(batchCwd);
  assert.equal(typeof agentTools.read_files.execute, "function");
  assert.equal(typeof agentTools.edit_files.execute, "function");
  const readOnlyTools = createCursorFilesystemTools(batchCwd, { readOnly: true });
  assert.equal(readOnlyTools.edit_files, undefined);
  const read = JSON.parse(await agentTools.read_files.execute({ paths: ["a.txt", "b.txt"] }));
  assert.deepEqual(read, [
    { path: "a.txt", content: "A\nconst quoted = \"x\";" },
    { path: "b.txt", content: "B\npath = C:\\tmp" },
  ]);
  await agentTools.edit_files.execute({
    files: [
      { path: "a.txt", edits: [{ oldLines: ["A", "const quoted = \"x\";"], newLines: ["AA", "const quoted = \"y\";"] }] },
      { path: "b.txt", edits: [{ oldLines: ["B", "path = C:\\tmp"], newLines: ["BB", "path = D:\\work"] }] },
    ],
  });
  assert.deepEqual(await Promise.all([
    readFile(join(batchCwd, "a.txt"), "utf8"),
    readFile(join(batchCwd, "b.txt"), "utf8"),
  ]), ["AA\nconst quoted = \"y\";", "BB\npath = D:\\work"]);
  await assert.rejects(() => agentTools.edit_files.execute({
    files: [
      { path: "a.txt", edits: [{ oldLines: ["AA"], newLines: ["changed"] }] },
      { path: "b.txt", edits: [{ oldLines: ["missing"], newLines: ["changed"] }] },
    ],
  }), /Could not find/);
  assert.deepEqual(await Promise.all([
    readFile(join(batchCwd, "a.txt"), "utf8"),
    readFile(join(batchCwd, "b.txt"), "utf8"),
  ]), ["AA\nconst quoted = \"y\";", "BB\npath = D:\\work"]);

  assert.equal(recommendEditFilesTransport([
    { path: "a.txt", edits: [{ oldLines: ["x"], newLines: ["y"] }] },
  ]), "files");
  assert.equal(recommendEditFilesTransport([
    { path: "a.go", edits: [{ oldLines: ["s := `json:\"x\"`"], newLines: ["s := `json:\"y\"`"] }] },
  ]), "filesB64");
  assert.equal(recommendEditFilesTransport([{
    path: "big.txt",
    edits: [{ oldLines: [Array(EDIT_FILES_INLINE_CHAR_LIMIT).fill("a").join("")], newLines: ["b"] }],
  }]), "filesB64");

  const b64Files = [
    { path: "a.txt", edits: [{ oldLines: ["AA", "const quoted = \"y\";"], newLines: ["AAA", "const quoted = \"z\";"] }] },
    { path: "b.txt", edits: [{ oldLines: ["BB", "path = D:\\work"], newLines: ["BBB", "path = E:\\work"] }] },
  ];
  const filesB64 = Buffer.from(JSON.stringify(b64Files), "utf8").toString("base64");
  assert.deepEqual(decodeFilesB64(filesB64), b64Files);
  assert.deepEqual(editFilesPathsFromArgs({ filesB64 }), ["a.txt", "b.txt"]);
  const b64Mapped = mapMessage({
    type: "tool_call",
    call_id: "edit-b64",
    name: "edit_files",
    status: "running",
    args: { filesB64 },
  }, createMessageState())[0];
  assert.deepEqual(b64Mapped.changes, [
    { path: "a.txt", kind: "update" },
    { path: "b.txt", kind: "update" },
  ]);
  await agentTools.edit_files.execute({ filesB64 });
  assert.deepEqual(await Promise.all([
    readFile(join(batchCwd, "a.txt"), "utf8"),
    readFile(join(batchCwd, "b.txt"), "utf8"),
  ]), ["AAA\nconst quoted = \"z\";", "BBB\npath = E:\\work"]);

  await assert.rejects(
    () => agentTools.edit_files.execute({ files: b64Files, filesB64 }),
    /only one of files or filesB64/,
  );
  await assert.rejects(
    () => agentTools.edit_files.execute({}),
    /exactly one of files or filesB64/,
  );

  await writeFile(join(batchCwd, "large.txt"), Array.from({ length: 250 }, (_, index) => `line-${index + 1}`).join("\n"));
  const first = JSON.parse(await agentTools.read_files.execute({ paths: ["large.txt"] }))[0];
  assert.equal(first.content.split("\n").length, 200);
  assert.equal(first.nextOffset, 201);
  const parent = await mkdtemp(join(tmpdir(), "nova-cursor-paths-"));
  const workspace = join(parent, "workspace");
  await mkdir(workspace);
  const outside = join(parent, "outside.txt");
  await writeFile(outside, "outside");
  const pathTools = createCursorFilesystemTools(workspace);
  assert.deepEqual(JSON.parse(await pathTools.read_files.execute({ paths: [outside] })), [
    { path: outside, content: "outside" },
  ]);
  await rm(parent, { recursive: true, force: true });
} finally {
  await rm(batchCwd, { recursive: true, force: true });
}
