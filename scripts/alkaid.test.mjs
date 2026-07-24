import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
import test from "node:test";
import { createCodingTools, createReadOnlyTools, getShellConfig } from "@earendil-works/pi-coding-agent";
import { alkaidDataRoot, alkaidModelOptions, mergeAlkaidCompatDefaults, mergeAlkaidConfig, parseJsonc, resolveAlkaidModel } from "./alkaid-config.mjs";
import { appendSlimTurn, compactSlimMemory, contextTokensFromMessages, createSlimMemory, formatSlimMemory, memoryWithoutCurrent, setLatestConclusion, shouldUseFullContext } from "./alkaid-slim-memory.mjs";
import {
  alkaidPromptInput,
  alkaidSkillsRoot,
  alkaidUserMessage,
  buildAlkaidSystemPrompt,
  clampOpenAIPayloadToolOutputs,
  clampPromptCacheKey,
  clampToolOutputText,
  connectMcpServers,
  createAlkaidAgent,
  createAlkaidIdleTimeout,
  createFilesystemTools,
  detectAlkaidShellConfig,
  expandAlkaidSkillCommand,
  findWindowsPowerShell,
  formatAlkaidSkillsPrompt,
  injectOpenAIPromptCacheKey,
  isRetryableAlkaidProviderError,
  loadAlkaidAgentInstructions,
  loadAlkaidSkills,
  mergeAlkaidUsage,
  OPENAI_TOOL_OUTPUT_MAX_CHARS,
  OPENAI_TOOL_OUTPUT_SAFE_MAX_CHARS,
  resolveAlkaidShellConfig,
  runAlkaidPromptWithRetry,
} from "./alkaid-core.mjs";
import { applySmartEdits } from "./alkaid-smart-edit.mjs";

const configuredModel = {
  id: "gpt-test",
  name: "GPT Test",
  api: "openai-responses",
  provider: "test",
  baseUrl: "http://127.0.0.1/v1",
  reasoning: true,
  input: ["text"],
  cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
  contextWindow: 128000,
  maxTokens: 32000,
};

test("OpenCode-style JSONC config resolves providers and models", () => {
  const config = {
    ...parseJsonc(`{
      // Alkaid provider
      "model": "custom/gpt-test",
      "provider": {
        "custom": {
          "npm": "@ai-sdk/openai-compatible",
          "name": "Custom",
          "options": { "baseURL": "http://127.0.0.1/v1", "apiKey": "{env:TEST_KEY}" },
          "models": {
            "gpt-test": {
              "name": "GPT Test",
              "reasoning": true,
              "modalities": { "input": ["text", "image"], "output": ["text"] },
              "limit": { "context": 200000, "output": 64000 },
              "options": { "reasoningEffort": "medium" },
              "variants": {
                "medium": { "reasoningEffort": "medium" },
                "high": { "reasoningEffort": "high" }
              },
            },
          },
        },
      },
    }`),
    env: { TEST_KEY: "secret" },
  };
  const resolved = resolveAlkaidModel(config);
  assert.equal(resolved.apiKey, "secret");
  assert.equal(resolved.model.api, "openai-completions");
  assert.equal(resolved.model.provider, "custom");
  assert.equal(resolved.model.contextWindow, 200000);
  assert.deepEqual(resolved.model.input, ["text", "image"]);
  assert.equal(resolved.thinkingLevel, "medium");
  assert.deepEqual(resolved.model.thinkingLevelMap, { medium: "medium", high: "high" });
  assert.equal(resolved.model.compat.sendSessionAffinityHeaders, true);
  assert.deepEqual(alkaidModelOptions(config), [
    {
      value: "custom/gpt-test/variant/medium",
      name: "Custom / GPT Test · Medium",
      _meta: { "codex.ai/supportsImages": true },
    },
    {
      value: "custom/gpt-test/variant/high",
      name: "Custom / GPT Test · High",
      _meta: { "codex.ai/supportsImages": true },
    },
  ]);
  assert.equal(resolveAlkaidModel(config, "custom/gpt-test/variant/high").thinkingLevel, "high");
  assert.throws(() => resolveAlkaidModel(config, "custom/gpt-test/variant/max"), /不支持思考强度/);
  delete config.model;
  assert.equal(resolveAlkaidModel(config).thinkingLevel, "medium");
});

test("server Alkaid config is merged in memory with local values winning", () => {
  const merged = mergeAlkaidConfig(
    {
      model: "server/server-model",
      provider: {
        server: { options: { baseURL: "https://server.example/v1", apiKey: "server-key" }, models: { "server-model": { name: "Server" } } },
        shared: { options: { baseURL: "https://server-shared/v1", apiKey: "server-key" }, models: { remote: { name: "Remote" }, same: { name: "Server Same" } } },
      },
    },
    {
      model: "shared/same",
      provider: {
        shared: { options: { apiKey: "local-key" }, models: { same: { name: "Local Same" }, local: { name: "Local" } } },
      },
    },
  );
  assert.equal(merged.model, "shared/same");
  assert.equal(merged.provider.shared.options.baseURL, "https://server-shared/v1");
  assert.equal(merged.provider.shared.options.apiKey, "local-key");
  assert.equal(merged.provider.shared.models.remote.name, "Remote");
  assert.equal(merged.provider.shared.models.same.name, "Local Same");
  assert.equal(merged.provider.shared.models.local.name, "Local");
  assert.equal(merged.provider.server.models["server-model"].name, "Server");
});

test("PI coding tools provide read, bash, edit and write", async () => {
  const cwd = await mkdtemp(join(tmpdir(), "alkaid-tools-"));
  const [read, bash, edit, write] = createCodingTools(cwd);
  assert.deepEqual([read.name, bash.name, edit.name, write.name], ["read", "bash", "edit", "write"]);
  assert.deepEqual(createReadOnlyTools(cwd).map((tool) => tool.name), ["read", "grep", "find", "ls"]);
  await write.execute("1", { path: "a.txt", content: "A" });
  assert.match((await read.execute("2", { path: "a.txt" })).content[0].text, /A/);
  await edit.execute("3", { path: "a.txt", edits: [{ oldText: "A", newText: "AA" }] });
  assert.match((await bash.execute("4", { command: "ls -1" })).content[0].text, /a\.txt/);
  assert.equal(await readFile(join(cwd, "a.txt"), "utf8"), "AA");
});

test("prompt input preserves embedded and local images", async () => {
  const cwd = await mkdtemp(join(tmpdir(), "alkaid-images-"));
  const localImage = join(cwd, "local.png");
  await writeFile(localImage, Buffer.from([1, 2, 3]));
  assert.deepEqual(await alkaidPromptInput([
    { type: "text", text: "inspect" },
    { type: "image_data", mime: "image/webp", data: "embedded" },
    { type: "local_image", path: localImage },
  ]), {
    text: "inspect",
    images: [
      { type: "image", data: "embedded", mimeType: "image/webp" },
      { type: "image", data: "AQID", mimeType: "image/png" },
    ],
  });
});

test("native steering messages preserve text and images", async () => {
  const message = await alkaidUserMessage([
    { type: "text", text: "change direction" },
    { type: "image_data", mime: "image/png", data: "image-data" },
  ]);
  assert.equal(message.role, "user");
  assert.deepEqual(message.content, [
    { type: "text", text: "change direction" },
    { type: "image", data: "image-data", mimeType: "image/png" },
  ]);
  assert.equal(typeof message.timestamp, "number");
});

test("Vega slim context keeps 10 conclusions and preserves interrupted prompts", async () => {
  const memory = createSlimMemory();
  for (let index = 1; index <= 10; index += 1) {
    appendSlimTurn(memory, `prompt ${index}`);
    setLatestConclusion(memory, [{ type: "text", text: `conclusion ${index}` }]);
  }
  assert.equal(await compactSlimMemory(memory, async () => assert.fail("10 turns must remain")), false);

  appendSlimTurn(memory, "interrupted prompt");
  appendSlimTurn(memory, "replacement prompt");
  const sentContext = formatSlimMemory(memoryWithoutCurrent(memory));
  assert.match(sentContext, /interrupted prompt/);
  assert.doesNotMatch(sentContext, /replacement prompt/);
  setLatestConclusion(memory, [{ type: "text", text: "latest conclusion" }]);

  let summaryInput = "";
  assert.equal(await compactSlimMemory(memory, async (input) => {
    summaryInput = input;
    return "older summary";
  }), true);
  assert.match(summaryInput, /prompt 1/);
  assert.doesNotMatch(summaryInput, /latest conclusion/);
  assert.equal(memory.summary, "older summary");
  assert.equal(memory.turns.length, 1);
  assert.deepEqual(memory.turns[0], {
    userPrompts: ["interrupted prompt", "replacement prompt"],
    conclusion: "latest conclusion",
  });
});

test("Vega slim context keeps an interrupted turn as native messages", () => {
  const memory = createSlimMemory();
  appendSlimTurn(memory, "completed prompt");
  setLatestConclusion(memory, "completed conclusion");
  appendSlimTurn(memory, "interrupted prompt");
  appendSlimTurn(memory, "current prompt");
  memory.pendingMessages = [
    { role: "user", content: [{ type: "text", text: "interrupted prompt" }] },
    { role: "assistant", content: [{ type: "toolCall", name: "read" }] },
    { role: "toolResult", content: [{ type: "text", text: "file contents" }] },
  ];

  const compactContext = formatSlimMemory(memoryWithoutCurrent(memory, { pendingMessages: true }));
  assert.match(compactContext, /completed conclusion/);
  assert.doesNotMatch(compactContext, /interrupted prompt|current prompt/);
  assert.equal(memory.pendingMessages[1].content[0].name, "read");
});

test("Vega slim context keeps complete early messages until its turn or token threshold", () => {
  const memory = createSlimMemory();
  memory.fullMessages = [{ role: "user", content: "full tool trajectory" }];
  memory.contextTokens = 749;
  appendSlimTurn(memory, "early prompt");
  assert.equal(shouldUseFullContext(memory, 1_000), true);
  memory.contextTokens = 750;
  assert.equal(shouldUseFullContext(memory, 750), false);
  memory.contextTokens = 1;
  while (memory.turns.length < 10) appendSlimTurn(memory, `prompt ${memory.turns.length}`);
  assert.equal(shouldUseFullContext(memory, 1_000), false);
  memory.contextStage = "slim";
  memory.contextTokens = 0;
  assert.equal(shouldUseFullContext(memory, 1_000), false);
});

test("Vega slim context uses separate thresholds for trajectory removal and summarization", async () => {
  const memory = createSlimMemory();
  for (let index = 1; index <= 11; index += 1) {
    appendSlimTurn(memory, `prompt ${index}`);
    setLatestConclusion(memory, `conclusion ${index}`);
  }
  memory.contextStage = "slim";
  memory.contextTokens = 0;

  assert.equal(await compactSlimMemory(memory, async () => assert.fail("stage one must not summarize"), {
    maxTurns: Number.POSITIVE_INFINITY,
    currentTokens: memory.contextTokens,
    maxTokens: 750,
  }), false);
  memory.contextTokens = 750;
  assert.equal(await compactSlimMemory(memory, async () => "stage two summary", {
    maxTurns: Number.POSITIVE_INFINITY,
    currentTokens: memory.contextTokens,
    maxTokens: 750,
  }), true);
  assert.equal(memory.summary, "stage two summary");
  assert.deepEqual(memory.turns.map((turn) => turn.conclusion), ["conclusion 11"]);
});

test("Vega slim context measures the largest native request instead of cumulative turn usage", () => {
  assert.equal(contextTokensFromMessages([
    { role: "assistant", usage: { input: 100, output: 20, cacheRead: 300, cacheWrite: 40 } },
    { role: "assistant", usage: { totalTokens: 900, input: 500, output: 30 } },
  ]), 900);
});

test("Vega slim context also compresses at the context character limit", async () => {
  const memory = createSlimMemory();
  for (let index = 1; index <= 3; index += 1) {
    appendSlimTurn(memory, `long prompt ${index} ${"x".repeat(100)}`);
    setLatestConclusion(memory, [{ type: "text", text: `conclusion ${index}` }]);
  }
  assert.equal(await compactSlimMemory(memory, async () => "size summary", { maxChars: 80 }), true);
  assert.equal(memory.summary, "size summary");
  assert.deepEqual(memory.turns.map((turn) => turn.conclusion), ["conclusion 3"]);
});

test("usage is accumulated across every model request in an agent turn", () => {
  const first = mergeAlkaidUsage(undefined, { input: 100, output: 20, cacheRead: 300, cacheWrite: 40 });
  const total = mergeAlkaidUsage(first, { input: 500, output: 30, cacheRead: 200, cacheWrite: 0 });
  assert.deepEqual(total, { input: 600, output: 50, cacheRead: 500, cacheWrite: 40 });
  assert.equal(mergeAlkaidUsage(undefined, undefined), undefined);
});

test("provider stream disconnects retry silently and preserve context", async () => {
  const user = { role: "user", content: [{ type: "text", text: "work" }], timestamp: Date.now() };
  const failed = { role: "assistant", content: [], stopReason: "error", errorMessage: "terminated" };
  const completed = { role: "assistant", content: [{ type: "text", text: "done" }], stopReason: "stop" };
  const retries = [];
  const agent = {
    state: { messages: [] },
    async prompt() { this.state.messages.push(user, failed); },
    async continue() { this.state.messages.push(completed); },
  };
  const outcome = await runAlkaidPromptWithRetry(agent, "work", [], {
    retryDelaysMs: [1000, 3000],
    sleep: async (delay) => retries.push(delay),
    onRetry: ({ attempt, error }) => retries.push(`${attempt}:${error}`),
  });
  assert.equal(outcome.last, completed);
  assert.equal(outcome.retries, 1);
  assert.deepEqual(agent.state.messages, [user, completed]);
  assert.deepEqual(retries, ["1:terminated", 1000]);
  assert.equal(isRetryableAlkaidProviderError("TypeError: terminated"), true);
  assert.equal(isRetryableAlkaidProviderError("HTTP 429 Too Many Requests"), true);
  assert.equal(isRetryableAlkaidProviderError("rate limit exceeded"), true);
  assert.equal(isRetryableAlkaidProviderError("HTTP 401 unauthorized"), false);
});

test("provider inactivity aborts and retries automatically", async () => {
  let aborts = 0;
  let continues = 0;
  const agent = {
    state: { messages: [] },
    async prompt() {
      this.state.messages.push(
        { role: "user", content: [], timestamp: Date.now() },
        { role: "assistant", content: [{ type: "thinking", thinking: "partial" }], stopReason: "toolUse" },
      );
      return new Promise(() => {});
    },
    async continue() {
      continues += 1;
      this.state.messages.push({ role: "assistant", content: [{ type: "text", text: "recovered" }], stopReason: "stop" });
    },
    abort() {
      aborts += 1;
      this.state.messages.push({ role: "assistant", content: [], stopReason: "aborted" });
    },
  };
  const idleTimeout = createAlkaidIdleTimeout({ timeoutMs: 10, onTimeout: () => agent.abort() });
  const retries = [];
  const outcome = await runAlkaidPromptWithRetry(agent, "work", [], {
    retryDelaysMs: [0],
    sleep: async () => {},
    runAttempt: (operation) => idleTimeout.run(operation),
    onRetry: ({ attempt, error }) => retries.push([attempt, error.name]),
  });
  assert.equal(outcome.last.content[0].text, "recovered");
  assert.equal(outcome.retries, 1);
  assert.equal(aborts, 1);
  assert.equal(continues, 1);
  assert.deepEqual(agent.state.messages.map((message) => message.role), ["user", "assistant"]);
  assert.deepEqual(retries, [[1, "AlkaidProviderIdleTimeoutError"]]);
});

test("provider inactivity waits for PI abort settlement before continuing", async () => {
  let settleAbort;
  let activeRun = Promise.resolve();
  const agent = {
    state: { messages: [] },
    prompt() {
      this.state.messages.push(
        { role: "user", content: [], timestamp: Date.now() },
        { role: "assistant", content: [{ type: "thinking", thinking: "partial" }], stopReason: "toolUse" },
      );
      activeRun = new Promise((resolve) => { settleAbort = resolve; });
      return activeRun;
    },
    abort() {
      setTimeout(() => {
        this.state.messages.push({ role: "assistant", content: [], stopReason: "aborted" });
        settleAbort();
      }, 0);
    },
    waitForIdle() {
      return activeRun;
    },
    async continue() {
      if (this.state.messages.at(-1)?.role === "assistant") {
        throw new Error("Cannot continue from message role: assistant");
      }
      this.state.messages.push({ role: "assistant", content: [{ type: "text", text: "recovered" }], stopReason: "stop" });
    },
  };
  const idleTimeout = createAlkaidIdleTimeout({ timeoutMs: 5, onTimeout: () => agent.abort() });
  const outcome = await runAlkaidPromptWithRetry(agent, "work", [], {
    retryDelaysMs: [0],
    sleep: async () => {},
    runAttempt: (operation) => idleTimeout.run(operation),
  });
  assert.equal(outcome.last.content[0].text, "recovered");
  assert.deepEqual(agent.state.messages.map((message) => message.role), ["user", "assistant"]);
});

test("provider activity resets the inactivity timeout", async () => {
  let timedOut = false;
  const idleTimeout = createAlkaidIdleTimeout({ timeoutMs: 20, onTimeout: () => { timedOut = true; } });
  const result = await idleTimeout.run(async () => {
    await new Promise((resolve) => setTimeout(resolve, 12));
    idleTimeout.touch();
    await new Promise((resolve) => setTimeout(resolve, 12));
    return "done";
  });
  assert.equal(result, "done");
  assert.equal(timedOut, false);
});

test("provider retries stop after the configured hidden attempts", async () => {
  const failures = ["fetch failed", "ECONNRESET", "terminated"];
  const user = { role: "user", content: [{ type: "text", text: "work" }], timestamp: Date.now() };
  const agent = {
    state: { messages: [] },
    async prompt() { this.state.messages.push(user, { role: "assistant", stopReason: "error", errorMessage: failures.shift() }); },
    async continue() { this.state.messages.push({ role: "assistant", stopReason: "error", errorMessage: failures.shift() }); },
  };
  const outcome = await runAlkaidPromptWithRetry(agent, "work", [], {
    retryDelaysMs: [0, 0],
    sleep: async () => {},
  });
  assert.equal(outcome.retries, 2);
  assert.equal(outcome.last.errorMessage, "terminated");
  assert.equal(agent.state.messages.length, 2);
});

test("batch file tools remain available as Alkaid enhancements", async () => {
  const cwd = await mkdtemp(join(tmpdir(), "alkaid-batch-"));
  await Promise.all([writeFile(join(cwd, "a.txt"), "A"), writeFile(join(cwd, "b.txt"), "B")]);
  const editTool = createCodingTools(cwd).find((tool) => tool.name === "edit");
  const [readFiles, editFiles] = createFilesystemTools(cwd, editTool);
  const read = await readFiles.execute("1", { paths: ["a.txt", "b.txt"] });
  assert.deepEqual(JSON.parse(read.content[0].text), [
    { path: "a.txt", content: "A" },
    { path: "b.txt", content: "B" },
  ]);
  await editFiles.execute("2", { files: [
    { path: "a.txt", edits: [{ oldText: "A", newText: "AA" }] },
    { path: "b.txt", edits: [{ oldText: "B", newText: "BB" }] },
  ] });
  assert.deepEqual(await Promise.all([readFile(join(cwd, "a.txt"), "utf8"), readFile(join(cwd, "b.txt"), "utf8")]), ["AA", "BB"]);
});

test("smart edits use normalized anchors and preserve the matched indentation", () => {
  const source = [
    "function outer() {",
    "    if (ready) {   ",
    "        log(“old”);",
    "    }",
    "}",
  ].join("\n");
  const result = applySmartEdits(source, [{
    oldText: "if (ready) {\n    log(\"old\");\n}",
    newText: "if (ready) {\n    log(\"new\");\n}",
  }], "sample.js");
  assert.equal(result.matches[0].mode, "relative-indent");
  assert.match(result.content, /    if \(ready\) \{\n        log\(\"new\"\);\n    \}/);
});

test("smart edits require the complete relative indentation shape", () => {
  const source = [
    "function outer() {",
    "      if (ready) {",
    "          work();",
    "      }",
    "}",
  ].join("\n");
  const result = applySmartEdits(source, [{
    oldText: "if (ready) {\n    work();\n}",
    newText: "if (ready) {\n    done();\n}",
  }], "relative.js");
  assert.equal(result.matches[0].mode, "relative-indent");
  assert.match(result.content, /      if \(ready\) \{\n          done\(\);\n      \}/);
});

test("smart edits reject fuzzy ambiguity", () => {
  const source = [
    "function first() {", "  calculate(invoiceSubtotal, regionalTax, shippingFee, discountCode, currencyA);", "}",
    "function second() {", "  calculate(invoiceSubtotal, regionalTax, shippingFee, discountCode, currencyB);", "}",
  ].join("\n");
  assert.throws(() => applySmartEdits(source, [{
    oldText: "calculate(invoiceSubtotal, regionalTax, shippingFee, discountCode, currencyC);",
    newText: "return total;",
  }], "ambiguous.js"), /Ambiguous fuzzy match/);
});

test("batch smart edits validate every file before writing", async () => {
  const cwd = await mkdtemp(join(tmpdir(), "alkaid-smart-transaction-"));
  await Promise.all([writeFile(join(cwd, "a.txt"), "alpha"), writeFile(join(cwd, "b.txt"), "beta")]);
  const editTool = createCodingTools(cwd).find((tool) => tool.name === "edit");
  const [, editFiles] = createFilesystemTools(cwd, editTool);
  await assert.rejects(() => editFiles.execute("1", { files: [
    { path: "a.txt", edits: [{ oldText: "alpha", newText: "changed" }] },
    { path: "b.txt", edits: [{ oldText: "missing", newText: "changed" }] },
  ] }), /Could not find/);
  assert.deepEqual(await Promise.all([readFile(join(cwd, "a.txt"), "utf8"), readFile(join(cwd, "b.txt"), "utf8")]), ["alpha", "beta"]);
});

test("batch reads stream large files in pages", async () => {
  const cwd = await mkdtemp(join(tmpdir(), "alkaid-batch-page-"));
  await writeFile(join(cwd, "large.txt"), Array.from({ length: 250 }, (_, index) => `line-${index + 1}`).join("\n"));
  const [readFiles] = createFilesystemTools(cwd);
  const first = JSON.parse((await readFiles.execute("1", { paths: ["large.txt"] })).content[0].text)[0];
  assert.equal(first.content.split("\n").length, 200);
  assert.equal(first.nextOffset, 201);
  assert.equal(first.truncated, true);
  const second = JSON.parse((await readFiles.execute("2", {
    paths: [{ path: "large.txt", offset: first.nextOffset, limit: 20 }],
  })).content[0].text)[0];
  assert.equal(second.content.split("\n").length, 20);
  assert.equal(second.content.split("\n")[0], "line-201");
  assert.equal(second.nextOffset, 221);
});

test("batch file tools allow absolute paths outside the workspace", async () => {
  const parent = await mkdtemp(join(tmpdir(), "alkaid-paths-"));
  const cwd = join(parent, "workspace");
  await mkdir(cwd);
  const outside = join(parent, "outside.txt");
  await writeFile(outside, "outside");
  await writeFile(join(cwd, "x.txt"), "x");

  const codingTools = createCodingTools(cwd);
  const nativeRead = codingTools.find((tool) => tool.name === "read");
  const editTool = codingTools.find((tool) => tool.name === "edit");
  const [readFiles, editFiles] = createFilesystemTools(cwd, editTool);
  assert.match((await nativeRead.execute("1", { path: outside })).content[0].text, /outside/);
  assert.deepEqual(JSON.parse((await readFiles.execute("2", { paths: [outside] })).content[0].text), [
    { path: outside, content: "outside" },
  ]);
  const absoluteInside = join(cwd, "x.txt");
  await editFiles.execute("3", { files: [
    { path: absoluteInside, edits: [{ oldText: "x", newText: "inside" }] },
  ] });
  assert.equal(await readFile(absoluteInside, "utf8"), "inside");
  await editFiles.execute("4", { files: [
    { path: outside, edits: [{ oldText: "outside", newText: "changed" }] },
  ] });
  assert.equal(await readFile(outside, "utf8"), "changed");
  await writeFile(absoluteInside, "alpha\nbeta\ngamma");
  await editFiles.execute("5", { files: [
    { path: "x.txt", edits: [{ oldText: "alpha", newText: "ALPHA" }] },
    { path: absoluteInside, edits: [{ oldText: "gamma", newText: "GAMMA" }] },
  ] });
  assert.equal(await readFile(absoluteInside, "utf8"), "ALPHA\nbeta\nGAMMA");
  await assert.rejects(() => editFiles.execute("6", { files: [
    { path: "x.txt", edits: [{ oldText: "ALPHA\nbeta", newText: "first" }] },
    { path: absoluteInside, edits: [{ oldText: "beta\nGAMMA", newText: "second" }] },
  ] }), /overlap/);
});

test("skills are discovered via pi loadSkillsFromDir", async () => {
  const root = await mkdtemp(join(tmpdir(), "nova-skills-test-"));
  const skillDir = join(root, "demo");
  await mkdir(skillDir);
  await writeFile(join(skillDir, "SKILL.md"), "---\nname: demo\ndescription: Demo skill\n---\nDo demo.");
  const { skills } = loadAlkaidSkills(root);
  assert.equal(skills.length, 1);
  assert.equal(skills[0].name, "demo");
  assert.equal(skills[0].description, "Demo skill");
  const prompt = formatAlkaidSkillsPrompt(skills);
  assert.match(prompt, /available_skills|<name>demo<\/name>|Demo skill/);
  assert.match(prompt, /read the SKILL\.md|Use the read tool/i);
});

test("slash skill commands expand to pi-compatible skill blocks", async () => {
  const root = await mkdtemp(join(tmpdir(), "nova-skill-command-test-"));
  const skillDir = join(root, "review");
  await mkdir(skillDir);
  await writeFile(join(skillDir, "SKILL.md"), "---\nname: review\ndescription: Review code\n---\nCheck correctness first.\n");
  const { skills } = loadAlkaidSkills(root);
  const expanded = await expandAlkaidSkillCommand("/skill:review focus on tests", skills);
  assert.match(expanded, /^<skill name="review" location=".*SKILL\.md">/);
  assert.match(expanded, /References are relative to .*review\./);
  assert.match(expanded, /Check correctness first\.\n<\/skill>\n\nfocus on tests$/);
  assert.equal(await expandAlkaidSkillCommand("/skill:missing keep", skills), "/skill:missing keep");
  assert.equal(await expandAlkaidSkillCommand("prefix /skill:review", skills), "prefix /skill:review");
});

test("many skills are compressed for prompt cache", () => {
  const skills = Array.from({ length: 4 }, (_, index) => ({
    name: `skill-${index}`,
    description: `Description ${index} `.repeat(20),
    filePath: `/home/user/.nova/alkaid/skills/skill-${index}/SKILL.md`,
    baseDir: `/home/user/.nova/alkaid/skills/skill-${index}`,
    sourceInfo: { source: "local", scope: "user" },
    disableModelInvocation: false,
  }));
  const verbose = formatAlkaidSkillsPrompt(skills.slice(0, 1));
  const compressed = formatAlkaidSkillsPrompt(skills);
  assert.match(verbose, /<available_skills>/);
  assert.match(compressed, /Skills under .*\/<name>\/SKILL\.md:/);
  assert.doesNotMatch(compressed, /<available_skills>/);
  assert.ok(compressed.length < verbose.length * 3);
});

test("Vega uses the data directory selected by the host build", () => {
  const env = { NOVA_DATA_DIR: join("/tmp", ".novadev") };
  assert.equal(alkaidDataRoot("/home/user", env), join("/tmp", ".novadev", "alkaid"));
  assert.equal(alkaidSkillsRoot("/home/user", env), join("/tmp", ".novadev", "alkaid", "skills"));
  assert.equal(alkaidDataRoot("/home/user", {}), join("/home/user", ".nova", "alkaid"));
});

test("Vega loads its managed AGENTS.md into the system prompt", async () => {
  const root = await mkdtemp(join(tmpdir(), "alkaid-agents-"));
  const instructionsPath = join(root, "AGENTS.md");
  await writeFile(instructionsPath, "Always answer in Chinese.\n", "utf8");
  assert.equal(await loadAlkaidAgentInstructions(instructionsPath), "Always answer in Chinese.\n");

  const runtime = await createAlkaidAgent({
    cwd: root,
    model: configuredModel,
    agentInstructionsPath: instructionsPath,
  });
  try {
    assert.match(runtime.agent.state.systemPrompt, /Always answer in Chinese\./);
  } finally {
    await runtime.close();
  }
});

test("system prompt keeps stable Alkaid policy before dynamic cwd/skills", () => {
  const prompt = buildAlkaidSystemPrompt({
    cwd: "D:/work/demo",
    skills: [{
      name: "demo",
      description: "Demo skill",
      filePath: "D:/skills/demo/SKILL.md",
      baseDir: "D:/skills/demo",
      sourceInfo: { source: "local" },
      disableModelInvocation: false,
    }],
    shellConfig: { shell: "/usr/bin/bash" },
  });
  const stableIndex = prompt.indexOf("你是 Vega");
  const separatorIndex = prompt.indexOf("\n---\n");
  const cwdIndex = prompt.indexOf("Current working directory:");
  assert.ok(stableIndex >= 0);
  assert.ok(separatorIndex > stableIndex);
  assert.ok(cwdIndex > separatorIndex);
  assert.match(prompt, /必须在一次 read_files 调用中合并读取/);
  assert.match(prompt, /禁止连续调用多个 read/);
});

test("openai prompt_cache_key fallback clamps session ids", () => {
  const longId = "s".repeat(80);
  assert.equal(clampPromptCacheKey(longId).length, 64);
  assert.deepEqual(injectOpenAIPromptCacheKey({ model: "x" }, "session-1"), {
    model: "x",
    prompt_cache_key: "session-1",
  });
  assert.equal(injectOpenAIPromptCacheKey({ prompt_cache_key: "keep" }, "session-1"), undefined);
});

test("clampToolOutputText keeps strings under the OpenAI max", () => {
  const huge = "x".repeat(OPENAI_TOOL_OUTPUT_MAX_CHARS + 1000);
  const clamped = clampToolOutputText(huge);
  assert.ok(clamped.length <= OPENAI_TOOL_OUTPUT_SAFE_MAX_CHARS);
  assert.match(clamped, /truncated: tool output exceeded/);
  assert.equal(clampToolOutputText("short"), "short");
});

test("clampOpenAIPayloadToolOutputs trims Responses and Completions tool outputs", () => {
  const huge = "y".repeat(OPENAI_TOOL_OUTPUT_MAX_CHARS + 500);
  const responses = clampOpenAIPayloadToolOutputs({
    model: "gpt",
    input: [
      { type: "message", role: "user", content: "hi" },
      { type: "function_call_output", call_id: "c1", output: huge },
      {
        type: "function_call_output",
        call_id: "c2",
        output: [{ type: "input_text", text: huge }],
      },
    ],
  });
  assert.ok(responses);
  assert.ok(responses.input[1].output.length <= OPENAI_TOOL_OUTPUT_SAFE_MAX_CHARS);
  assert.ok(responses.input[2].output[0].text.length <= OPENAI_TOOL_OUTPUT_SAFE_MAX_CHARS);

  const completions = clampOpenAIPayloadToolOutputs({
    messages: [
      { role: "user", content: "hi" },
      { role: "tool", tool_call_id: "t1", content: huge },
    ],
  });
  assert.ok(completions);
  assert.ok(completions.messages[1].content.length <= OPENAI_TOOL_OUTPUT_SAFE_MAX_CHARS);
  assert.equal(clampOpenAIPayloadToolOutputs({ input: [{ type: "function_call_output", output: "ok" }] }), undefined);
});

test("batch reads truncate oversized lines by byte budget", async () => {
  const cwd = await mkdtemp(join(tmpdir(), "alkaid-batch-bytes-"));
  const hugeLine = "z".repeat(80 * 1024);
  await writeFile(join(cwd, "wide.txt"), `${hugeLine}\nline-2`);
  const [readFiles] = createFilesystemTools(cwd);
  const first = JSON.parse((await readFiles.execute("1", { paths: ["wide.txt"] })).content[0].text)[0];
  assert.equal(first.truncated, true);
  assert.ok(Buffer.byteLength(first.content, "utf8") <= 50 * 1024);
  assert.ok(first.content.length < hugeLine.length);
  assert.equal(first.nextOffset, 2);
});

test("compat defaults enable session affinity for openai-compatible proxies", () => {
  const compat = mergeAlkaidCompatDefaults(
    "openai-completions",
    "deepseek-chat",
    "https://proxy.example/v1",
    undefined,
  );
  assert.equal(compat.sendSessionAffinityHeaders, true);
  assert.equal(compat.thinkingFormat, "deepseek");
  assert.equal(compat.requiresReasoningContentOnAssistantMessages, true);
  assert.deepEqual(
    mergeAlkaidCompatDefaults("openai-completions", "gpt", "https://proxy.example/v1", {
      sendSessionAffinityHeaders: false,
    }),
    { sendSessionAffinityHeaders: false },
  );
});

test("MCP stdio tools are discovered and callable", async () => {
  const mcp = await connectMcpServers({
    echo: { command: process.execPath, args: [join(process.cwd(), "scripts/fixtures/mcp-echo-server.mjs")] },
  });
  try {
    assert.equal(mcp.tools[0].name, "mcp__echo__echo");
    const result = await mcp.tools[0].execute("1", { text: "nova" });
    assert.equal(result.content[0].text, "echo:nova");
  } finally {
    await mcp.close();
  }
});

test("plan mode exposes no write tool", async () => {
  const cwd = await mkdtemp(join(tmpdir(), "alkaid-plan-"));
  const runtime = await createAlkaidAgent({ cwd, readOnly: true, model: configuredModel });
  try {
    assert.equal(runtime.agent.state.thinkingLevel, "off");
    assert.deepEqual(runtime.agent.state.tools.slice(0, 5).map((tool) => tool.name), ["read_files", "read", "grep", "find", "ls"]);
    assert(!runtime.agent.state.tools.some((tool) => tool.name === "edit_files"));
  } finally {
    await runtime.close();
  }
});

test("Windows shell shim overrides Alkaid's absolute shell path", () => {
  const bash = { shell: "C:\\Program Files\\Git\\bin\\bash.exe", args: ["-c"] };
  const bashShim = "C:\\Nova\\runtime\\windows-shell-shim\\bash.exe";
  assert.deepEqual(
    resolveAlkaidShellConfig(bash, { NOVA_SHELL_SHIM_BASH: bashShim }, "win32"),
    { ...bash, shell: bashShim });
  assert.equal(resolveAlkaidShellConfig(bash, { NOVA_SHELL_SHIM_BASH: bashShim }, "linux"), bash);

  const powershell = { shell: "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe", args: ["-c"], kind: "powershell" };
  const psShim = "C:\\Nova\\runtime\\windows-shell-shim\\powershell.exe";
  assert.deepEqual(
    resolveAlkaidShellConfig(powershell, { NOVA_SHELL_SHIM_POWERSHELL: psShim }, "win32"),
    { ...powershell, shell: psShim });
  assert.equal(resolveAlkaidShellConfig(powershell, { NOVA_SHELL_SHIM_POWERSHELL: psShim }, "linux"), powershell);
});

test("Windows shell detection uses PowerShell without requiring bash", async () => {
  const root = await mkdtemp(join(tmpdir(), "alkaid-pshell-"));
  const psDir = join(root, "System32", "WindowsPowerShell", "v1.0");
  await mkdir(psDir, { recursive: true });
  const ps = join(psDir, "powershell.exe");
  await writeFile(ps, "ps");
  const config = detectAlkaidShellConfig({ SystemRoot: root, PATH: "" }, "win32");
  assert.equal(config.kind, "powershell");
  assert.equal(config.shell, ps);
});

test("findWindowsPowerShell falls back to PATH lookup", async () => {
  const dir = await mkdtemp(join(tmpdir(), "alkaid-pspath-"));
  const ps = join(dir, "powershell.exe");
  await writeFile(ps, "ps");
  assert.equal(findWindowsPowerShell({ PATH: dir }), ps);
  assert.equal(findWindowsPowerShell({ PATH: "" }), null);
});

test("Windows PowerShell prompt instructs PowerShell syntax", () => {
  const prompt = buildAlkaidSystemPrompt({
    cwd: tmpdir(),
    shellConfig: { shell: "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe", args: ["-c"], kind: "powershell" },
  });
  assert(prompt.includes("命令终端已确认使用 PowerShell"));
  assert(prompt.includes("- bash: 执行 PowerShell 命令"));
  assert(!prompt.includes("不要使用 PowerShell cmdlet"));
});

test("build mode confirms and uses the detected Bash shell", async () => {
  const cwd = await mkdtemp(join(tmpdir(), "alkaid-shell-"));
  const shellConfig = getShellConfig();
  const resolvedShellConfig = resolveAlkaidShellConfig(shellConfig);
  const runtime = await createAlkaidAgent({ cwd, model: configuredModel, shellConfig });
  try {
    assert.equal(runtime.agent.steeringMode, "all");
    assert.deepEqual(runtime.agent.state.tools.slice(0, 2).map((tool) => tool.name), ["read_files", "edit_files"]);
    assert(!runtime.agent.state.tools.some((tool) => tool.name === "write_files"));
    assert(!runtime.agent.state.tools.some((tool) => tool.name === "load_skill"));
    assert.match(runtime.agent.state.systemPrompt, /读取内容遵循最小必要原则.*已知目标行范围时，只读取相关行段/);
    assert.match(runtime.agent.state.systemPrompt, /未知目标位置时，先用搜索工具定位行号/);
    assert.match(runtime.agent.state.systemPrompt, /两个及以上路径已知.*必须在一次 read_files 调用中合并读取/);
    assert.match(runtime.agent.state.systemPrompt, /禁止连续调用多个 read/);
    assert.match(runtime.agent.state.systemPrompt, /禁止用并行封装的多个 read 代替 read_files/);
    assert.match(runtime.agent.state.systemPrompt, /按顺序理解文件不构成读取依赖/);
    assert.match(runtime.agent.state.systemPrompt, /后续新发现多个独立文本目标.*仍须合并使用 read_files/);
    assert.match(runtime.agent.state.systemPrompt, /为每个文件分别设置必要的 offset\/limit/);
    assert.match(runtime.agent.state.systemPrompt, /禁止使用 `grep -r` 或 `grep -R`.*无排除的递归搜索/);
    assert.match(runtime.agent.state.systemPrompt, /优先使用 `git grep`.*未跟踪文件时使用 `rg`.*遵守 `\.gitignore`/);
    assert.match(runtime.agent.state.systemPrompt, /输出截断只限制结果展示，不属于工作量限制/);
    assert.match(runtime.agent.state.systemPrompt, /递归命令必须通过限定路径.*设置较短的 timeout/);
    assert.match(runtime.agent.state.systemPrompt, /递归命令超时后不得原样重试/);
    assert.match(runtime.agent.state.systemPrompt, /优先根据版本控制 diff 按需确定受影响单元及直接使用方/);
    assert.match(runtime.agent.state.systemPrompt, /禁止遍历或列出完整仓库、无依据扩大范围/);
    assert(runtime.agent.state.systemPrompt.includes(`命令终端已确认使用 Bash（${resolvedShellConfig.shell}）`));
    assert.match(runtime.agent.state.systemPrompt, /不要使用 PowerShell cmdlet/);
    assert.match(runtime.agent.state.systemPrompt, /\n---\n/);
    assert.ok(runtime.agent.state.systemPrompt.indexOf("你是 Vega") < runtime.agent.state.systemPrompt.indexOf("Current working directory:"));
    const bash = runtime.agent.state.tools.find((tool) => tool.name === "bash");
    assert.match((await bash.execute("1", { command: "printf 'shell-ok'" })).content[0].text, /shell-ok/);
  } finally {
    await runtime.close();
  }
});

test("bridge aborts cleanly and persists resumable context", async () => {
  const home = await mkdtemp(join(tmpdir(), "alkaid-abort-"));
  const dataRoot = join(home, ".nova", "alkaid");
  await mkdir(dataRoot, { recursive: true });
  const server = createServer(() => {});
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const port = server.address().port;
  await writeFile(join(dataRoot, "config.jsonc"), JSON.stringify({
    model: "test/model/variant/low",
    provider: {
      test: {
        npm: "@ai-sdk/openai-compatible",
        options: { baseURL: `http://127.0.0.1:${port}/v1`, apiKey: "test" },
        models: {
          model: {
            reasoning: true,
            limit: { context: 10000, output: 1000 },
            variants: { low: { reasoningEffort: "low" } },
          },
        },
      },
    },
  }));
  const child = spawn(process.execPath, [join(process.cwd(), "scripts/alkaid-bridge.mjs")], {
    cwd: process.cwd(),
    env: { ...process.env, HOME: home, USERPROFILE: home, NOVA_DATA_DIR: join(home, ".nova") },
    stdio: ["pipe", "pipe", "pipe"],
  });
  const events = [];
  let stderr = "";
  child.stderr.on("data", (chunk) => { stderr += chunk; });
  let cancelStartedAt;
  const output = createInterface({ input: child.stdout, crlfDelay: Infinity });
  output.on("line", (line) => {
    const event = JSON.parse(line);
    events.push(event);
    if (event.type === "ready") {
      cancelStartedAt = Date.now();
      child.stdin.write(`${JSON.stringify({ action: "cancel" })}\n`);
    }
  });
  child.stdin.write(`${JSON.stringify({
    action: "prompt",
    cwd: process.cwd(),
    sessionId: "abort-test",
    model: "test/model/variant/low",
    parts: [{ type: "text", text: "wait" }],
  })}\n`);
  let timeout;
  const exitCode = await Promise.race([
    new Promise((resolve) => child.once("exit", resolve)),
    new Promise((_, reject) => {
      timeout = setTimeout(() => {
        child.kill();
        reject(new Error("Alkaid bridge startup or cancel timed out"));
      }, 15000);
    }),
  ]).finally(() => clearTimeout(timeout));
  await new Promise((resolve) => server.close(resolve));
  assert.equal(exitCode, 0, `events: ${JSON.stringify(events)}\nstderr: ${stderr}`);
  assert(Date.now() - cancelStartedAt < 1000);
  assert(events.some((event) => event.type === "done" && event.cancelled === true));
  const messages = JSON.parse(await readFile(join(dataRoot, "sessions", "abort-test.json"), "utf8"));
  assert.equal(messages[0].role, "user");
  assert.equal(messages.at(-1).stopReason, "aborted");
});
