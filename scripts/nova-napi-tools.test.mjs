import assert from "node:assert/strict";
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
  const learningDir = await mkdtemp(join(tmpdir(), "nova-context-learning-model-"));
  const previousDir = process.env.NOVA_CONTEXT_LEARNING_DIR;
  const previousEnabled = process.env.NOVA_CONTEXT_LEARNING;
  process.env.NOVA_CONTEXT_LEARNING_DIR = learningDir;
  process.env.NOVA_CONTEXT_LEARNING = "1";
  try {
    await writeFile(join(root, "target.ts"), "export function target() {\n  return 1;\n}\n");
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
    assert.ok(settled.observations >= 1);
    const files = await import("node:fs/promises").then(({ readdir }) => readdir(learningDir));
    assert.equal(files.filter((file) => file.endsWith(".json")).length, 1);
  } finally {
    if (previousDir === undefined) delete process.env.NOVA_CONTEXT_LEARNING_DIR;
    else process.env.NOVA_CONTEXT_LEARNING_DIR = previousDir;
    if (previousEnabled === undefined) delete process.env.NOVA_CONTEXT_LEARNING;
    else process.env.NOVA_CONTEXT_LEARNING = previousEnabled;
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
