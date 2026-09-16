import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import ts from "typescript";

// Run the actual startup registration phase with held IPC replies: a serial await
// would leave only the first listener registered and fail without timing assertions.
const source = readFileSync(new URL("../src/store.ts", import.meta.url), "utf8");
const ast = ts.createSourceFile("store.ts", source, ts.ScriptTarget.Latest, true);
const init = ast.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === "initStore");
const barrier = init.body.statements.find((node) => node.getText(ast).startsWith("const settingsResult ="));
const code = source.slice(init.getStart(ast), barrier.getStart(ast)) + "return await settingsReady; }";
const pending = new Map();
const events = [];
const settings = { modelFavorites: [], theme: "ink-dark" };
const noop = () => {};
const context = vm.createContext({
  initialized: false,
  initRoamingWorkflows: async () => {},
  setInterval: noop,
  RATE_WINDOW_MS: 1000,
  listen: (event) => {
    events.push(event);
    return new Promise((resolve) => pending.set(event, resolve));
  },
  api: {
    getSettings: async () => { events.push("settings"); return settings; },
    checkUpdate: async () => { events.push("update"); return { hasUpdate: false }; },
  },
  modelFavoriteIds: () => [],
  lastUsed: { agentKind: () => "lyra" },
  agentEnabled: () => true,
  isThemePref: () => true,
  state: { theme: "ink-dark" },
  setState: noop,
  ensureModelOptions: noop,
});
vm.runInContext(ts.transpile(code.replace("export ", ""), {
  target: ts.ScriptTarget.ES2022,
  module: ts.ModuleKind.None,
}), context);
const tick = () => new Promise((resolve) => setImmediate(resolve));
let settled = false;
const startup = context.initStore().then((value) => { settled = true; return value; });
await tick();
assert.deepEqual(events, ["acp:options"]);
pending.get("acp:options")();
await tick();
assert.equal(pending.size, 33, "all remaining listeners must register without waiting for IPC replies");
assert.equal(events[1], "settings", "model listener must be ready before settings starts model refresh");
assert.equal(settled, false);
for (const [event, resolve] of pending) if (event !== "relay:quota-progress") resolve();
await tick();
assert.equal(settled, false, "startup must await even the last listener before reading snapshots");
assert.equal(events.includes("update"), false);
pending.get("relay:quota-progress")();
assert.equal((await startup).settings, settings);
assert.equal(events.includes("update"), true);
const registrations = events.length;
await context.initStore();
assert.equal(events.length, registrations, "repeated initialization must not register twice");
console.log("Startup registration: concurrency, event ordering and one-time initialization passed.");
