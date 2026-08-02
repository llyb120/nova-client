import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import {
  createNovaBatchTools,
  normalizeFastContextArgs,
  normalizeFindSymbolsArgs,
  novaDevinBatchToolPolicy,
} from "./nova-batch-tools.mjs";

test("createNovaBatchTools exposes read/context/edit by default", () => {
  const prev = process.env.NOVA_FAST_CONTEXT;
  const prevRo = process.env.NOVA_TOOLS_READ_ONLY;
  delete process.env.NOVA_FAST_CONTEXT;
  delete process.env.NOVA_TOOLS_READ_ONLY;
  try {
    const tools = createNovaBatchTools(process.cwd(), { includeEditFiles: true });
    assert.equal(typeof tools.read_files.execute, "function");
    assert.equal(typeof tools.fast_context.execute, "function");
    assert.equal(typeof tools.find_symbols.execute, "function");
    assert.equal(typeof tools.edit_files.execute, "function");
  } finally {
    if (prev === undefined) delete process.env.NOVA_FAST_CONTEXT;
    else process.env.NOVA_FAST_CONTEXT = prev;
    if (prevRo === undefined) delete process.env.NOVA_TOOLS_READ_ONLY;
    else process.env.NOVA_TOOLS_READ_ONLY = prevRo;
  }
});

test("NOVA_FAST_CONTEXT=0 omits context tools", () => {
  const prev = process.env.NOVA_FAST_CONTEXT;
  process.env.NOVA_FAST_CONTEXT = "0";
  try {
    const tools = createNovaBatchTools(process.cwd(), { includeEditFiles: true });
    assert.equal(tools.fast_context, undefined);
    assert.equal(tools.find_symbols, undefined);
    assert.equal(typeof tools.read_files.execute, "function");
    assert.equal(typeof tools.edit_files.execute, "function");
  } finally {
    if (prev === undefined) delete process.env.NOVA_FAST_CONTEXT;
    else process.env.NOVA_FAST_CONTEXT = prev;
  }
});

test("read-only omits edit_files", () => {
  const tools = createNovaBatchTools(process.cwd(), { readOnly: true, includeEditFiles: true });
  assert.equal(tools.edit_files, undefined);
  assert.match(novaDevinBatchToolPolicy({ readOnly: true }), /plan\/read-only/);
});

test("context tool schemas accept common model argument aliases", () => {
  const tools = createNovaBatchTools(process.cwd(), { fastContext: true, includeEditFiles: true });
  assert.deepEqual(normalizeFastContextArgs({ query: "cursor" }), {
    query: "cursor", keywords: ["cursor"], task: "", files: [],
  });
  assert.deepEqual(normalizeFastContextArgs({ query: "how cursor connects" }).task, "how cursor connects");
  assert.deepEqual(normalizeFindSymbolsArgs({ query: "CursorAdapter" }), { names: ["CursorAdapter"] });
  assert.deepEqual(normalizeFindSymbolsArgs({ symbols: ["A", "A", "B"] }), { names: ["A", "B"] });
  assert.equal(tools.fast_context.inputSchema.required, undefined);
  assert.equal(tools.find_symbols.inputSchema.required, undefined);
  assert.equal(tools.fast_context.inputSchema.anyOf.length, 4);
  assert.equal(tools.find_symbols.inputSchema.anyOf.length, 5);
});

test("read_files accepts paths and common files alias; edit_files round-trips", async () => {
  const dir = mkdtempSync(join(tmpdir(), "nova-tools-"));
  try {
    writeFileSync(join(dir, "a.txt"), "hello\n");
    writeFileSync(join(dir, "b.txt"), "world\n");
    const tools = createNovaBatchTools(dir, { fastContext: false, includeEditFiles: true });
    assert.equal(tools.read_files.inputSchema.required, undefined);
    assert.equal(tools.read_files.inputSchema.anyOf.length, 2);
    const read = JSON.parse(await tools.read_files.execute({ paths: ["a.txt", "b.txt"] }));
    assert.equal(read[0].content, "hello");
    assert.equal(read[1].content, "world");
    const aliased = JSON.parse(await tools.read_files.execute({ files: [{ path: "a.txt", offset: 1, limit: 1 }] }));
    assert.equal(aliased[0].content, "hello");
    await tools.edit_files.execute({
      files: [{ path: "a.txt", edits: [{ oldText: "hello", newText: "hola" }] }],
    });
    assert.equal(readFileSync(join(dir, "a.txt"), "utf8"), "hola\n");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("devin policy mentions nova-tools and read_files", () => {
  const prev = process.env.NOVA_FAST_CONTEXT;
  delete process.env.NOVA_FAST_CONTEXT;
  try {
    const policy = novaDevinBatchToolPolicy({ includeEditFiles: true });
    assert.match(policy, /nova-tools/);
    assert.match(policy, /read_files/);
    assert.match(policy, /edit_files/);
    assert.match(policy, /fast_context/);
    assert.match(policy, /ROUTING RULE — before choosing any tool/);
    assert.match(policy, /must NEVER be selected as direct tool calls/);
    assert.match(policy, /only valid execution path/);
    assert.match(policy, /generic mcp_call_tool wrapper/);
    assert.match(policy, /Set server_name to the top-level string "nova-tools"/);
    assert.match(policy, /"server_name":"nova-tools"/);
    assert.match(policy, /wording such as `use\/call read_files`/);
    assert.match(policy, /do not call mcp_list_tools merely to discover them/);
    assert.match(policy, /Never repeat a malformed call unchanged/);
    assert.match(policy, /never invent arbitrary 100\/200-line pages/);
    assert.match(policy, /Never sequentially page through a whole file/);
  } finally {
    if (prev === undefined) delete process.env.NOVA_FAST_CONTEXT;
    else process.env.NOVA_FAST_CONTEXT = prev;
  }
});
