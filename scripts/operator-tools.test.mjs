import test from "node:test";
import assert from "node:assert/strict";
import { createNovaBatchTools } from "./nova-batch-tools.mjs";

function withEnv(values, fn) {
  const before = {};
  for (const [key, value] of Object.entries(values)) {
    before[key] = process.env[key];
    if (value == null) delete process.env[key];
    else process.env[key] = value;
  }
  try { return fn(); }
  finally {
    for (const [key, value] of Object.entries(before)) {
      if (value == null) delete process.env[key];
      else process.env[key] = value;
    }
  }
}

function hasDescription(value) {
  if (Array.isArray(value)) return value.some(hasDescription);
  if (!value || typeof value !== "object") return false;
  if (Object.hasOwn(value, "description")) return true;
  return Object.values(value).some(hasDescription);
}

test("parent session exposes operator alongside raw GUI tools", () => withEnv({
  NOVA_CONTEXT_SERVICE_ENDPOINT: "test-endpoint",
  NOVA_CONTEXT_SERVICE_TOKEN: "test-token",
  NOVA_PARENT_THREAD_ID: "parent-1",
  NOVA_OPERATOR_CHILD: null,
  NOVA_FAST_CONTEXT: "0",
}, () => {
  const tools = createNovaBatchTools(process.cwd(), { fastContext: false });
  assert.ok(tools.operator);
  assert.ok(tools.chrome);
  assert.ok(tools.jianlai);
  assert.ok(tools.webview);
}));

test("operator child exposes only compact chrome and jianlai", () => withEnv({
  NOVA_CONTEXT_SERVICE_ENDPOINT: "test-endpoint",
  NOVA_CONTEXT_SERVICE_TOKEN: "test-token",
  NOVA_PARENT_THREAD_ID: "operator-child",
  NOVA_OPERATOR_CHILD: "1",
  NOVA_FAST_CONTEXT: "1",
}, () => {
  const tools = createNovaBatchTools(process.cwd(), { fastContext: true });
  assert.deepEqual(Object.keys(tools).sort(), ["chrome", "jianlai"]);
  assert.equal(hasDescription(tools.chrome.inputSchema), false);
  assert.equal(hasDescription(tools.jianlai.inputSchema), false);
  assert.match(tools.chrome.description, /禁止盲目重放/);
}));

test("operator is not exposed without a Nova parent thread identity", () => withEnv({
  NOVA_CONTEXT_SERVICE_ENDPOINT: "test-endpoint",
  NOVA_CONTEXT_SERVICE_TOKEN: "test-token",
  NOVA_PARENT_THREAD_ID: null,
  NOVA_OPERATOR_CHILD: null,
  NOVA_FAST_CONTEXT: "0",
}, () => {
  const tools = createNovaBatchTools(process.cwd(), { fastContext: false });
  assert.equal(tools.operator, undefined);
}));
