import assert from "node:assert/strict";
import { test } from "node:test";
import {
  createNovaBatchTools,
  normalizePolarisArgs,
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
  assert.deepEqual(Object.keys(tools).sort(), ["polaris"]);
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
  assert.deepEqual(normalizePolarisArgs({ query: "Widget" }).keywords, ["Widget"]);
});

test("polaris keywords normalize to top five", () => {
  assert.deepEqual(normalizePolarisArgs({ keywords: "Widget" }).keywords, ["Widget"]);
  assert.deepEqual(
    normalizePolarisArgs({ keywords: ["a", "b", "a", "c", "d", "e", "f"] }).keywords,
    ["a", "b", "c", "d", "e"],
  );
  const schema = withContextService(() => createNovaBatchTools(process.cwd(), { fastContext: true })).polaris.inputSchema.properties.keywords;
  assert.equal(schema.maxItems, undefined);
  assert(schema.anyOf.some((option) => option.type === "string"));
});

test("CodeBuddy direct mode describes polaris as a direct tool", () => {
  const previous = process.env.NOVA_MCP_DIRECT;
  process.env.NOVA_MCP_DIRECT = "1";
  try {
    const tool = withContextService(() => createNovaBatchTools(process.cwd(), { fastContext: true }).polaris);
    assert.match(tool.description, /直接调用/);
    assert.doesNotMatch(tool.description, /mcp_call_tool/);
    assert.doesNotMatch(tool.description, /ToolSearch|DeferExecuteTool/);
  } finally {
    if (previous === undefined) delete process.env.NOVA_MCP_DIRECT;
    else process.env.NOVA_MCP_DIRECT = previous;
  }
});

test("devin policy routes context through MCP when the native service exists", () => {
  const policy = withContextService(() => novaDevinBatchToolPolicy({ fastContext: true }));
  assert.match(policy, /mcp_call_tool/);
  assert.match(policy, /polaris/);
  assert.match(policy, /Devin native edit tools/);
});
