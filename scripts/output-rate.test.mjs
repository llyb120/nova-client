// Run: node scripts/output-rate.test.mjs
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import ts from "typescript";

const source = readFileSync(new URL("../src/store.ts", import.meta.url), "utf8");
const rates = source.slice(source.indexOf("const RATE_WINDOW_MS"), source.indexOf("function clearDeltaRate"));
const clear = source.slice(source.indexOf("function clearDeltaRate"), source.indexOf("function clearDeltaRate") + source.slice(source.indexOf("function clearDeltaRate")).indexOf("\n}") + 2);
const upsert = source.slice(source.indexOf("function applyUpsert"), source.indexOf("function applyUpsert") + source.slice(source.indexOf("function applyUpsert")).indexOf("\n}") + 2);
let now = 0;
const state = { currentId: "a", running: { a: true }, items: [] };
const context = vm.createContext({
  performance: { now: () => now }, state,
  createSignal: (value) => [() => value, (update) => { value = update(value); }],
  reconcile: (value) => value,
  setState: (_key, index, value) => { state.items[index] = value; },
});
vm.runInContext(ts.transpile(`${rates}\n${clear}\n${upsert}`.replace(/export /g, ""), { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None }), context);
const run = (code) => vm.runInContext(code, context);
const item = (text) => run(`applyUpsert({ type: "assistant", id: 1, text: ${JSON.stringify(text)} })`);
item("a".repeat(200));
now = 500;
run('trackDeltaRate("a", 0)');
assert.equal(run('outputRates().a'), 100, "first full-text update contributes output");
item("a".repeat(200));
now = 1000;
run('trackDeltaRate("a", 0)');
assert.equal(run('outputRates().a'), 0, "duplicate snapshot and idle do not generate tokens");
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
assert.equal(run('outputRates().a'), 0, "threads have independent windows");
run('clearDeltaRate("a"); clearDeltaRate("b")');
assert.equal(run('Object.keys(outputRates()).length'), 0, "turn cleanup removes rates");
console.log("output rate checks passed");
