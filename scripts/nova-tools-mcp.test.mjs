import assert from "node:assert/strict";
import { test } from "node:test";
import {
  createNovaBatchTools,
  normalizeFastContextArgs,
  normalizeFindSymbolsArgs,
  novaDevinBatchToolPolicy,
} from "./nova-batch-tools.mjs";

function withContextService(run) {
  const previousEndpoint = process.env.NOVA_CONTEXT_SERVICE_ENDPOINT;
  const previousToken = process.env.NOVA_CONTEXT_SERVICE_TOKEN;
  process.env.NOVA_CONTEXT_SERVICE_ENDPOINT = "test-endpoint";
  process.env.NOVA_CONTEXT_SERVICE_TOKEN = "test-token";
  try { return run(); }
  finally {
    if (previousEndpoint === undefined) delete process.env.NOVA_CONTEXT_SERVICE_ENDPOINT;
    else process.env.NOVA_CONTEXT_SERVICE_ENDPOINT = previousEndpoint;
    if (previousToken === undefined) delete process.env.NOVA_CONTEXT_SERVICE_TOKEN;
    else process.env.NOVA_CONTEXT_SERVICE_TOKEN = previousToken;
  }
}

test("createNovaBatchTools exposes context tools only with the native service", () => {
  const previousEndpoint = process.env.NOVA_CONTEXT_SERVICE_ENDPOINT;
  const previousToken = process.env.NOVA_CONTEXT_SERVICE_TOKEN;
  delete process.env.NOVA_CONTEXT_SERVICE_ENDPOINT;
  delete process.env.NOVA_CONTEXT_SERVICE_TOKEN;
  try { assert.deepEqual(createNovaBatchTools(process.cwd(), { fastContext: true }), {}); }
  finally {
    if (previousEndpoint !== undefined) process.env.NOVA_CONTEXT_SERVICE_ENDPOINT = previousEndpoint;
    if (previousToken !== undefined) process.env.NOVA_CONTEXT_SERVICE_TOKEN = previousToken;
  }
  const tools = withContextService(() => createNovaBatchTools(process.cwd(), { fastContext: true }));
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
  const schema = withContextService(() => createNovaBatchTools(process.cwd(), { fastContext: true })).fast_context.inputSchema.properties.keywords;
  assert.equal(schema.maxItems, undefined);
  assert(schema.anyOf.some((option) => option.type === "string"));
});

test("devin policy routes context through MCP when the native service exists", () => {
  const policy = withContextService(() => novaDevinBatchToolPolicy({ fastContext: true }));
  assert.match(policy, /mcp_call_tool/);
  assert.match(policy, /fast_context/);
  assert.match(policy, /Devin native edit tools/);
});
