import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { callNapiTool, callNapiToolOrFallback, napiToolsAvailable } from "./nova-napi-tools.mjs";

test("N-API addon serves read_files and edit_files", { skip: !napiToolsAvailable() }, async () => {
  const root = await mkdtemp(join(tmpdir(), "nova-napi-tools-"));
  try {
    await writeFile(join(root, "a.ts"), "export function target() {\n  return 1;\n}\n");
    const read = await callNapiTool("read_files", root, { paths: [{ path: "a.ts", limit: 2 }] });
    assert.match(read[0].content, /export function target/);

    const edited = await callNapiTool("edit_files", root, {
      files: [{ path: "a.ts", edits: [{ oldText: "return 1", newText: "return 2" }] }],
    });
    assert.deepEqual(edited.paths, ["a.ts"]);
    assert.match(await readFile(join(root, "a.ts"), "utf8"), /return 2/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("fast_context online learning consumes edit feedback automatically", { skip: !napiToolsAvailable() }, async () => {
  const root = await mkdtemp(join(tmpdir(), "nova-context-learning-root-"));
  const worktree = `${root}-linked`;
  const learningDir = await mkdtemp(join(tmpdir(), "nova-context-learning-model-"));
  const previousDir = process.env.NOVA_CONTEXT_LEARNING_DIR;
  const previousEnabled = process.env.NOVA_CONTEXT_LEARNING;
  const previousOwner = process.env.NOVA_CONTEXT_LEARNING_OWNER;
  process.env.NOVA_CONTEXT_LEARNING_DIR = learningDir;
  process.env.NOVA_CONTEXT_LEARNING = "1";
  process.env.NOVA_CONTEXT_LEARNING_OWNER = "1";
  try {
    await writeFile(join(root, "target.ts"), "export function target() {\n  return 1;\n}\n");
    execFileSync("git", ["init", "-q"], { cwd: root });
    execFileSync("git", ["config", "user.email", "nova-test@example.invalid"], { cwd: root });
    execFileSync("git", ["config", "user.name", "Nova Test"], { cwd: root });
    execFileSync("git", ["add", "target.ts"], { cwd: root });
    execFileSync("git", ["commit", "-qm", "seed"], { cwd: root });
    execFileSync("git", ["worktree", "add", "-q", "-b", "linked", worktree], { cwd: root });
    const context = await callNapiTool("fast_context", root, {
      keywords: ["target"],
      files: ["target.ts"],
      task: "change target implementation",
    });
    assert.match(context, /target\.ts/);
    const feedback = await callNapiTool("observe_context_feedback", root, {
      action: "edit",
      path: "target.ts",
    });
    assert.equal(feedback.updated, 1);
    const settled = await callNapiTool("observe_context_feedback", root, { action: "settle" });
    assert.equal(settled.settled, 1);

    // linked worktree 必须复用同一内存状态和同一个持久化模型文件。
    await callNapiTool("fast_context", worktree, {
      keywords: ["target"],
      files: ["target.ts"],
      task: "change target from linked worktree",
    });
    const linkedFeedback = await callNapiTool("observe_context_feedback", worktree, {
      action: "edit",
      path: "target.ts",
    });
    assert.equal(linkedFeedback.updated, 1);
    await callNapiTool("observe_context_feedback", worktree, { action: "settle" });
    const files = await import("node:fs/promises").then(({ readdir }) => readdir(learningDir));
    assert.equal(files.filter((file) => file.endsWith(".json")).length, 1);
  } finally {
    if (previousDir === undefined) delete process.env.NOVA_CONTEXT_LEARNING_DIR;
    else process.env.NOVA_CONTEXT_LEARNING_DIR = previousDir;
    if (previousEnabled === undefined) delete process.env.NOVA_CONTEXT_LEARNING;
    else process.env.NOVA_CONTEXT_LEARNING = previousEnabled;
    if (previousOwner === undefined) delete process.env.NOVA_CONTEXT_LEARNING_OWNER;
    else process.env.NOVA_CONTEXT_LEARNING_OWNER = previousOwner;
    await rm(worktree, { recursive: true, force: true });
    await rm(root, { recursive: true, force: true });
    await rm(learningDir, { recursive: true, force: true });
  }
});

test("JS backend mode bypasses the addon", async () => {
  const previous = process.env.NOVA_TOOLS_BACKEND;
  process.env.NOVA_TOOLS_BACKEND = "js";
  try {
    const result = await callNapiToolOrFallback("read_files", process.cwd(), {}, async () => "fallback");
    assert.equal(result, "fallback");
  } finally {
    if (previous === undefined) delete process.env.NOVA_TOOLS_BACKEND;
    else process.env.NOVA_TOOLS_BACKEND = previous;
  }
});
