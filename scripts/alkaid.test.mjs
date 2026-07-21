import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { connectMcpServers, createAlkaidAgent, createFilesystemTools, createSkillSupport } from "./alkaid-core.mjs";

test("read_files and write_files handle batches", async () => {
  const cwd = await mkdtemp(join(tmpdir(), "alkaid-test-"));
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

test("filesystem tools reject traversal and duplicate writes", async () => {
  const cwd = await mkdtemp(join(tmpdir(), "alkaid-test-"));
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
  const runtime = await createAlkaidAgent({ cwd, readOnly: true });
  try {
    assert(!runtime.agent.state.tools.some((tool) => tool.name === "write_files"));
    assert(runtime.agent.state.tools.some((tool) => tool.name === "read_files"));
  } finally {
    await runtime.close();
  }
});
