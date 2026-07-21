import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
import test from "node:test";
import { createCodingTools, createReadOnlyTools, getShellConfig } from "@earendil-works/pi-coding-agent";
import { alkaidModelOptions, parseJsonc, resolveAlkaidModel } from "./alkaid-config.mjs";
import { connectMcpServers, createAlkaidAgent, createFilesystemTools, createSkillSupport } from "./alkaid-core.mjs";

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

test("batch file tools remain available as Alkaid enhancements", async () => {
  const cwd = await mkdtemp(join(tmpdir(), "alkaid-batch-"));
  await Promise.all([writeFile(join(cwd, "a.txt"), "A"), writeFile(join(cwd, "b.txt"), "B")]);
  const [readFiles, writeFiles] = createFilesystemTools(cwd);
  const read = await readFiles.execute("1", { paths: ["a.txt", "b.txt"] });
  assert.deepEqual(JSON.parse(read.content[0].text), [
    { path: "a.txt", content: "A" },
    { path: "b.txt", content: "B" },
  ]);
  await writeFiles.execute("2", { files: [{ path: "a.txt", content: "AA" }, { path: "b.txt", content: "BB" }] });
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

test("batch file tools reject traversal and duplicate writes", async () => {
  const cwd = await mkdtemp(join(tmpdir(), "alkaid-batch-"));
  const [, writeFiles] = createFilesystemTools(cwd);
  await assert.rejects(() => writeFiles.execute("1", { files: [{ path: "../outside", content: "x" }] }), /超出工作区/);
  await assert.rejects(() => writeFiles.execute("2", { files: [{ path: "x", content: "1" }, { path: "x", content: "2" }] }), /重复/);
});

test("skills are discovered and loadable", async () => {
  const root = await mkdtemp(join(tmpdir(), "nova-skills-test-"));
  const skillDir = join(root, "demo");
  await mkdir(skillDir);
  await writeFile(join(skillDir, "SKILL.md"), "---\nname: demo\ndescription: Demo skill\n---\nDo demo.");
  const support = await createSkillSupport(root);
  assert(support.catalog.includes("demo: Demo skill"));
  const loaded = await support.tool.execute("1", { name: "demo" });
  assert(loaded.content[0].text.includes("Do demo."));
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
    assert.deepEqual(runtime.agent.state.tools.slice(0, 4).map((tool) => tool.name), ["read", "grep", "find", "ls"]);
    assert(runtime.agent.state.tools.some((tool) => tool.name === "read_files"));
    assert(!runtime.agent.state.tools.some((tool) => tool.name === "write_files"));
  } finally {
    await runtime.close();
  }
});

test("build mode confirms and uses the detected Bash shell", async () => {
  const cwd = await mkdtemp(join(tmpdir(), "alkaid-shell-"));
  const shellConfig = getShellConfig();
  const runtime = await createAlkaidAgent({ cwd, model: configuredModel, shellConfig });
  try {
    assert(runtime.agent.state.systemPrompt.includes(`命令终端已确认使用 Bash（${shellConfig.shell}）`));
    assert.match(runtime.agent.state.systemPrompt, /不要使用 PowerShell cmdlet/);
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
        reject(new Error("Alkaid bridge cancel timed out"));
      }, 5000);
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
