// Run: node --test scripts/fork-thread.test.mjs
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { createEffect, createMemo, createRoot, createSignal, onCleanup } from "solid-js/dist/solid.js";
import { createStore } from "solid-js/store/dist/store.js";

const read = (path) => readFileSync(new URL(path, import.meta.url), "utf8");
const chat = read("../src/components/ChatView.tsx");
const compile = (source) => ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
}).outputText;
function load(path, dependencies, globals = {}) {
  const exports = {};
  vm.runInNewContext(compile(read(path)), { exports, require: (name) => {
    assert.ok(name in dependencies, `unexpected dependency: ${name}`);
    return dependencies[name];
  }, ...globals });
  return exports;
}
function between(start, end) {
  const from = chat.indexOf(start), to = chat.indexOf(end, from);
  assert.ok(from >= 0 && to > from);
  return chat.slice(from, to);
}
const logic = compile(
  between("  const stageIndex =", "  const stageThreads =") +
  between("  const [forkSourceId,", "  // worktree 会话的 cwd"),
);
function mount(t, { fork = async () => ({ id: "copy-stage" }), refresh = async () => {} } = {}) {
  const [state, setState] = createStore({ currentId: "stage", settings: { sessionShortcuts: [] }, running: { stage: true }, threads: [
    { id: "root", title: "整链根标题" },
    { id: "stage", title: "子 Agent", parentThreadId: "root", subagent: true },
    { id: "sibling", title: "兄弟", parentThreadId: "root" },
    { id: "other", title: "另一会话" },
  ] });
  const calls = [];
  const { api } = load("../src/ipc.ts", { "@tauri-apps/api/core": { invoke: async (command, args) => {
    calls.push([command, { ...args }]);
    assert.equal(command, "fork_thread", "fork must not rename, stop, or send a prompt");
    return fork(args);
  } } });
  let keydown, stops = 0;
  const shortcuts = load("../src/sessionShortcuts.ts", {
    "solid-js": { onMount: (fn) => fn(), onCleanup: () => {} },
    "./store": { state, ALL_AGENT_KINDS: [] },
  }, { HTMLElement: class {}, window: { addEventListener: (_, fn) => { keydown = fn; } } });
  shortcuts.mountSessionShortcuts({ allowedActions: ["stopSession"], onStopSession: () => { stops++; return true; } });
  const ui = createRoot((dispose) => {
    t.after(dispose);
    return new Function("createSignal", "createMemo", "createEffect", "onCleanup", "state", "currentMeta", "api", "refreshThreads", "openThread", "setShortcutCaptureActive", `${logic}
      return { startFork, closeFork, submitFork, forkSourceId, forkTitle, setForkTitle, forkBusy, forkError, forkedId, forkUnavailable };`
    )(createSignal, createMemo, createEffect, onCleanup, state, () => state.threads.find((thread) => thread.id === state.currentId), api,
      async () => { calls.push(["refresh"]); await refresh(); },
      async (id) => { calls.push(["open", id]); }, shortcuts.setShortcutCaptureActive);
  });
  return { ...ui, state, setState, calls, stops: () => stops,
    escape: () => keydown({ key: "Escape", target: null, preventDefault() {}, stopPropagation() {} }),
  };
}

test("fork IPC uses frozen Stage id and root default title; running/subagent records remain forkable", async (t) => {
  let resolve;
  const ui = mount(t, { fork: () => new Promise((done) => { resolve = done; }) });
  const original = JSON.stringify(ui.state.threads);
  ui.startFork();
  assert.equal(ui.forkTitle(), "整链根标题 (fork)");
  ui.setForkTitle("  独立副本  ");
  ui.setState("currentId", "other");
  ui.startFork();
  const pending = ui.submitFork();
  ui.closeFork();
  await ui.submitFork();
  assert.equal(ui.forkSourceId(), "stage");
  assert.equal(ui.forkBusy(), true);
  assert.deepEqual(ui.calls, [["fork_thread", { threadId: "stage", title: "独立副本" }]]);
  resolve({ id: "copy-stage" });
  await pending;
  assert.deepEqual(ui.calls.slice(1), [["refresh"], ["open", "copy-stage"]]);
  assert.equal(ui.forkSourceId(), null);
  assert.equal(ui.forkBusy(), false);
  assert.equal(JSON.stringify(ui.state.threads), original);
  assert.equal(ui.state.running.stage, true);
});

test("blank names do not submit; rejection keeps dialog/input; refresh retry does not duplicate a created copy", async (t) => {
  let rejectFork = true, rejectRefresh = true;
  const ui = mount(t, {
    fork: async () => { if (rejectFork) throw new Error("整链不允许 fork"); return { id: "copy-stage" }; },
    refresh: async () => { if (rejectRefresh) throw new Error("列表刷新失败"); },
  });
  ui.startFork();
  ui.setForkTitle(" \t ");
  await ui.submitFork();
  assert.deepEqual(ui.calls, []);
  ui.setForkTitle("保留输入");
  await ui.submitFork();
  assert.equal(ui.forkSourceId(), "stage");
  assert.equal(ui.forkTitle(), "保留输入");
  assert.equal(ui.forkBusy(), false);
  assert.match(ui.forkError(), /整链不允许 fork/);
  rejectFork = false;
  await ui.submitFork();
  assert.equal(ui.forkedId(), "copy-stage");
  assert.equal(ui.forkTitle(), "保留输入");
  assert.match(ui.forkError(), /副本已创建.*列表刷新失败/);
  rejectRefresh = false;
  await ui.submitFork();
  assert.equal(ui.calls.filter(([command]) => command === "fork_thread").length, 2);
  assert.deepEqual(ui.calls.at(-1), ["open", "copy-stage"]);
  assert.equal(ui.forkSourceId(), null);
});

test("roaming/quota nodes are blocked; modal Escape cannot stop the running original", (t) => {
  const ui = mount(t);
  for (const patch of [{ roamingRole: "guest" }, { roamingRole: "host" }, { roamingRole: null, quotaPeer: "peer" }]) {
    ui.setState("threads", 1, patch);
    assert.equal(ui.forkUnavailable(), true);
    ui.startFork();
    assert.equal(ui.forkSourceId(), null);
  }
  ui.setState("threads", 1, { roamingRole: null, quotaPeer: null });
  ui.startFork();
  ui.escape();
  assert.equal(ui.stops(), 0);
  ui.closeFork();
  assert.equal(ui.forkSourceId(), null);
  ui.escape();
  assert.equal(ui.stops(), 1, "normal session shortcuts are restored after closing");
});

test("dialog contracts: native modal focus, name input, Enter/IME, Escape/cancel, scope disclosure", () => {
  const dialog = between("      <Show when={forkSourceId()}>", "      <Show when={!isSubagent() && showShare()");
  for (const pattern of [/<Portal>/, /<dialog/, /aria-labelledby="fork-dialog-title"/, /aria-describedby="fork-dialog-description"/,
    /dialog\.showModal\(\)/, /onCleanup\(\(\) => dialog\.close\(\)\)/, /autofocus/, /<form/, /onSubmit=.*submitFork\(\)/,
    /onCancel=.*preventDefault\(\); closeFork\(\)/, /event\.key === "Enter" && event\.isComposing/,
    /type="button".*onClick={closeFork}>取消/, /type="submit".*disabled={forkBusy\(\) \|\| !forkTitle\(\)\.trim\(\)}/,
    /role="alert"/, /全部 Stage/, /原会话和正在运行的任务不受影响/, /同一工作目录，不复制文件/]) assert.match(dialog, pattern);
  const button = between("        <Show when={currentMeta()}>", "        <Show when={state.relay.connected && state.currentId && roamingRole()");
  assert.match(button, /disabled={forkUnavailable\(\)}/);
  assert.match(button, /onClick={startFork}/);
  assert.doesNotMatch(button, /isSubagent|isRunning/);
});
