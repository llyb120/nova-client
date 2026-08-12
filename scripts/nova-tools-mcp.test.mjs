import assert from "node:assert/strict";
import { test } from "node:test";
import {
  createNovaBatchTools,
  normalizeFastContextArgs,
  normalizeFindSymbolsArgs,
  novaDevinBatchToolPolicy,
} from "./nova-batch-tools.mjs";

test("createNovaBatchTools exposes only enabled context tools", () => {
  const tools = createNovaBatchTools(process.cwd(), { fastContext: true });
  assert.deepEqual(Object.keys(tools).sort(), ["fast_context", "find_symbols"]);
});

test("NOVA_FAST_CONTEXT=0 omits context tools", () => {
  const previous = process.env.NOVA_FAST_CONTEXT;
  process.env.NOVA_FAST_CONTEXT = "0";
  try { assert.deepEqual(createNovaBatchTools(process.cwd()), {}); }
  finally {
    if (previous === undefined) delete process.env.NOVA_FAST_CONTEXT;
    else process.env.NOVA_FAST_CONTEXT = previous;
  }
});

test("context argument aliases normalize", () => {
  assert.deepEqual(normalizeFindSymbolsArgs({ query: "Widget" }), { names: ["Widget"] });
  assert.deepEqual(normalizeFastContextArgs({ query: "Widget" }).keywords, ["Widget"]);
});

test("fast_context keywords normalize to top five", () => {
  assert.deepEqual(normalizeFastContextArgs({ keywords: "Widget" }).keywords, ["Widget"]);
  assert.deepEqual(
    normalizeFastContextArgs({ keywords: ["a", "b", "a", "c", "d", "e", "f"] }).keywords,
    ["a", "b", "c", "d", "e"],
  );
  const schema = createNovaBatchTools(process.cwd(), { fastContext: true }).fast_context.inputSchema.properties.keywords;
  assert.equal(schema.maxItems, undefined);
  assert(schema.anyOf.some((option) => option.type === "string"));
});

test("devin policy routes context through MCP", () => {
  const policy = novaDevinBatchToolPolicy({ fastContext: true });
  assert.match(policy, /mcp_call_tool/);
  assert.match(policy, /fast_context/);
  assert.match(policy, /Devin native edit tools/);
});
