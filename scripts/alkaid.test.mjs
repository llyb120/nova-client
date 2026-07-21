import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { createCodingTools, createReadOnlyTools } from "@earendil-works/pi-coding-agent";
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
              "variants": { "high": { "reasoningEffort": "high" } },
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
  assert.deepEqual(resolved.model.thinkingLevelMap, { high: "high" });
  assert.deepEqual(alkaidModelOptions(config), [{
    value: "custom/gpt-test",
    name: "Custom / GPT Test",
    _meta: { "codex.ai/supportsImages": true },
  }]);
  delete config.model;
  assert.equal(resolveAlkaidModel(config).model.id, "gpt-test");
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
  const support = await createSkillSupport([root]);
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
    assert.deepEqual(runtime.agent.state.tools.slice(0, 4).map((tool) => tool.name), ["read", "grep", "find", "ls"]);
    assert(runtime.agent.state.tools.some((tool) => tool.name === "read_files"));
    assert(!runtime.agent.state.tools.some((tool) => tool.name === "write_files"));
  } finally {
    await runtime.close();
  }
});
