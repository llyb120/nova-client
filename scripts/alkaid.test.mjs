import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
import test from "node:test";
import { createCodingTools, createReadOnlyTools, getShellConfig } from "@earendil-works/pi-coding-agent";
import { alkaidModelOptions, mergeAlkaidCompatDefaults, mergeAlkaidConfig, parseJsonc, resolveAlkaidModel } from "./alkaid-config.mjs";
import {
  alkaidPromptInput,
  alkaidUserMessage,
  buildAlkaidSystemPrompt,
  clampPromptCacheKey,
  connectMcpServers,
  createAlkaidAgent,
  createFilesystemTools,
  formatAlkaidSkillsPrompt,
  injectOpenAIPromptCacheKey,
  isRetryableAlkaidProviderError,
  loadAlkaidSkills,
  resolveAlkaidShellConfig,
  runAlkaidPromptWithRetry,
} from "./alkaid-core.mjs";

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
  assert.equal(isRetryableAlkaidProviderError("HTTP 401 unauthorized"), false);
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

test("batch file tools reject traversal and duplicate edits", async () => {
  const cwd = await mkdtemp(join(tmpdir(), "alkaid-batch-"));
  await writeFile(join(cwd, "x.txt"), "x");
  const editTool = createCodingTools(cwd).find((tool) => tool.name === "edit");
  const [, editFiles] = createFilesystemTools(cwd, editTool);
  await assert.rejects(() => editFiles.execute("1", { files: [
    { path: "../outside", edits: [{ oldText: "x", newText: "y" }] },
  ] }), /超出工作区/);
  await assert.rejects(() => editFiles.execute("2", { files: [
    { path: "x.txt", edits: [{ oldText: "x", newText: "1" }] },
    { path: "x.txt", edits: [{ oldText: "x", newText: "2" }] },
  ] }), /重复/);
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
  const stableIndex = prompt.indexOf("你是 Alkaid");
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

test("Windows shell shim overrides Alkaid's absolute Bash path", () => {
  const detected = { shell: "C:\\Program Files\\Git\\bin\\bash.exe", args: ["-c"] };
  const shim = "C:\\Nova\\runtime\\windows-shell-shim\\bash.exe";
  const resolved = resolveAlkaidShellConfig(detected, { NOVA_SHELL_SHIM_BASH: shim });
  if (process.platform === "win32") assert.deepEqual(resolved, { ...detected, shell: shim });
  else assert.equal(resolved, detected);
});

test("build mode confirms and uses the detected Bash shell", async () => {
  const cwd = await mkdtemp(join(tmpdir(), "alkaid-shell-"));
  const shellConfig = getShellConfig();
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
    assert.match(runtime.agent.state.systemPrompt, /识别可独立验证的工程单元及其依赖关系/);
    assert.match(runtime.agent.state.systemPrompt, /不得用一个单元的验证代替其他受影响单元/);
    assert(runtime.agent.state.systemPrompt.includes(`命令终端已确认使用 Bash（${shellConfig.shell}）`));
    assert.match(runtime.agent.state.systemPrompt, /不要使用 PowerShell cmdlet/);
    assert.match(runtime.agent.state.systemPrompt, /\n---\n/);
    assert.ok(runtime.agent.state.systemPrompt.indexOf("你是 Alkaid") < runtime.agent.state.systemPrompt.indexOf("Current working directory:"));
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
    env: { ...process.env, HOME: home, USERPROFILE: home },
    stdio: ["pipe", "pipe", "pipe"],
  });
  const events = [];
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
  assert.equal(exitCode, 0);
  assert(Date.now() - cancelStartedAt < 1000);
  assert(events.some((event) => event.type === "done" && event.cancelled === true));
  const messages = JSON.parse(await readFile(join(dataRoot, "sessions", "abort-test.json"), "utf8"));
  assert.equal(messages[0].role, "user");
  assert.equal(messages.at(-1).stopReason, "aborted");
});
