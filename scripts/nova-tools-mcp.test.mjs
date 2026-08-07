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

test("devin policy routes context through MCP", () => {
  const policy = novaDevinBatchToolPolicy({ fastContext: true });
  assert.match(policy, /mcp_call_tool/);
  assert.match(policy, /fast_context/);
  assert.match(policy, /Devin native edit tools/);
});
