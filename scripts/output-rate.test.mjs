// Run: node scripts/output-rate.test.mjs
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import ts from "typescript";
import { batch, createComputed, createRoot, createSignal } from "solid-js/dist/solid.js";

const source = readFileSync(new URL("../src/store.ts", import.meta.url), "utf8");
const rates = source.slice(source.indexOf("const RATE_WINDOW_MS"), source.indexOf("function clearDeltaRate"));
const clear = source.slice(source.indexOf("function clearDeltaRate"), source.indexOf("function clearDeltaRate") + source.slice(source.indexOf("function clearDeltaRate")).indexOf("\n}") + 2);
const upsert = source.slice(source.indexOf("function applyUpsert"), source.indexOf("function applyUpsert") + source.slice(source.indexOf("function applyUpsert")).indexOf("\n}") + 2);
let now = 0;
const state = { currentId: "a", running: { a: true }, items: [] };
const context = vm.createContext({
  performance: { now: () => now }, state,
  createSignal,
  reconcile: (value) => value,
  setState: (_key, index, value) => { state.items[index] = value; },
});
vm.runInContext(ts.transpile(`${rates}\n${clear}\n${upsert}`.replace(/export /g, ""), { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None }), context);
const run = (code) => vm.runInContext(code, context);
const item = (text) => run(`applyUpsert({ type: "assistant", id: 1, text: ${JSON.stringify(text)} })`);
let displayed = -1;
const dispose = createRoot((dispose) => {
  createComputed(() => { displayed = run('getOutputRate("a")'); });
  return dispose;
});
assert.equal(displayed, 0);
item("a".repeat(200));
now = 500;
run('trackDeltaRate("a", 0)');
assert.equal(run('outputRates().a'), 100, "first full-text update contributes output");
assert.equal(displayed, 100, "UI subscription updates from initial zero");
item("a".repeat(200));
now = 1000;
run('trackDeltaRate("a", 0)');
assert.equal(displayed, 100, "brief empty window does not erase a burst");
assert.equal(run('rateWindows.get("a").chars'), 0, "duplicate snapshot adds no output");
run('trackDeltaRate("a", 200)');
state.items[0].text += "b".repeat(200); // flushed delta
item(state.items[0].text);
now = 1500;
run('trackDeltaRate("a", 0)');
assert.equal(run('outputRates().a'), 100, "final snapshot does not count delta twice");
run('trackDeltaRate("b", 400)');
now = 2000;
run('trackDeltaRate("b", 0); trackDeltaRate("a", 0)');
assert.equal(run('outputRates().b'), 200);
assert.equal(run('outputRates().a'), 100, "threads have independent windows");
now = 2500;
run('trackDeltaRate("a", 0)');
assert.equal(displayed, 0, "idle zero is published to the UI");
run('trackDeltaRate("a", 200)');
now = 3000;
run('trackDeltaRate("a", 0)');
assert.equal(displayed, 100, "same-speed output resumes reactively after idle");
run('clearDeltaRate("a"); clearDeltaRate("b")');
assert.equal(run('Object.keys(outputRates()).length'), 0, "turn cleanup removes rates");
now = 4000;
run('trackDeltaRate("a", 800)');
now = 6000; // delayed WebView timer: the first nonzero sample must still reach the UI
run('trackDeltaRate("a", 0)');
assert.equal(displayed, 100, "delayed first sample is not hidden by a getter timeout");
now = 6500;
run('trackDeltaRate("a", 0)');
assert.equal(displayed, 0);
dispose();

// Replay the actual acp:update listener and stream batching, not just the sampler.
const file = ts.createSourceFile("store.ts", source, ts.ScriptTarget.Latest, true);
const init = file.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === "initStore");
let listener;
function findListener(node) {
  if (ts.isCallExpression(node) && node.expression.getText(file) === "listen"
    && node.arguments?.[0]?.text === "acp:update") listener = node;
  ts.forEachChild(node, findListener);
}
findListener(init);
const timer = init.body.statements.find((node) => ts.isExpressionStatement(node)
  && node.expression.expression?.getText(file) === "setInterval");
assert.ok(listener && timer);
for (const backend of ["lyra", "codebuddy"]) {
  let receive, tick;
  const view = { currentId: backend, running: { [backend]: true }, items: [], loadingThread: false };
  const runtime = vm.createContext({
    performance: { now: () => now }, state: view, createSignal, batch,
    reconcile: (value) => value, produce: (update) => update,
    setState: (key, index, value) => {
      if (typeof index === "function") index(view[key]);
      else if (value !== undefined) view[key][index] = value;
      else view[key] = index;
    },
    window: { setTimeout: () => 1, clearTimeout() {} },
    setInterval: (callback) => { tick = callback; },
    listen: async (_event, callback) => { receive = callback; },
    liveUsageByThread: new Map(), threadSnapshots: new Map(), staleThreadSnapshots: new Set(),
    snapshotToolUpdates: undefined,
  });
  const stream = source.slice(source.indexOf("const pendingDeltas ="), source.indexOf("let initialized ="));
  const code = `${stream}\n${timer.getText(file)}\n(async () => { await ${listener.getText(file)} })()`;
  await vm.runInContext(ts.transpile(code.replace(/export /g, ""), { target: ts.ScriptTarget.ES2022 }), runtime);
  let visibleRate = 0;
  const stop = createRoot((stop) => {
    createComputed(() => { visibleRate = vm.runInContext(`getOutputRate("${backend}")`, runtime); });
    return stop;
  });
  now = 10_000;
  const full = (text) => ({ t: "upsert", item: { type: "assistant", id: 1, text } });
  receive({ payload: { threadId: backend, op: full("a".repeat(100)) } });
  now += 250;
  receive({ payload: { threadId: backend, ops: [backend === "lyra"
    ? { t: "delta", itemId: 1, text: "b".repeat(100) }
    : full("a".repeat(100) + "b".repeat(100))] } });
  now += 250;
  tick();
  assert.equal(visibleRate, 100, `${backend}: real listener and timer update the UI`);
  receive({ payload: { threadId: backend, op: full("a".repeat(100) + "b".repeat(100)) } });
  now += 2000;
  tick();
  assert.equal(visibleRate, 0, `${backend}: final snapshot is not counted twice`);
  assert.equal(view.items[0].text.length, 200);
  // Tool arguments are generated output (Claude Code's input_json_delta);
  // tool results and repeated completion snapshots must not become token speed.
  now = 20_000;
  const tool = { type: "tool", id: 2, status: "in_progress", rawInput: "x".repeat(200), content: [] };
  receive({ payload: { threadId: backend, op: { t: "upsert", item: tool } } });
  vm.runInContext("flushPendingStreamUpdates()", runtime);
  now += 500;
  tick();
  assert.equal(visibleRate, 100, `${backend}: 200 new argument chars / 4 / 0.5s = 100 tok/s`);
  receive({ payload: { threadId: backend, op: { t: "upsert", item: {
    ...tool, status: "completed", rawOutput: "log".repeat(100_000),
  } } } });
  now += 2000;
  tick();
  assert.equal(visibleRate, 0, `${backend}: tool results add no generated tokens`);
  stop();
}
console.log("output rate checks passed");
