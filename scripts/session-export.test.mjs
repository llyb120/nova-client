import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import ts from "typescript";

const chat = readFileSync(new URL("../src/components/ChatView.tsx", import.meta.url), "utf8");
const source = chat.slice(chat.indexOf("  const [exportBusy,"), chat.indexOf("  const [forkSourceId,"));
const compiled = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText;
function mount({ save = async () => "chosen.zip", invoke = async () => {} } = {}) {
  const state = { currentId: "original" }, calls = [], notices = [];
  const signal = value => [() => value, next => { value = next; }];
  const ui = new Function("createSignal", "state", "save", "api", "message", `${compiled}; return { exportSession, exportBusy };`)(
    signal, state, save,
    { exportThreadDiagnostics: async (...args) => { calls.push(args); await invoke(...args); } },
    async (...args) => { notices.push(args); },
  );
  return { ...ui, state, calls, notices };
}

test("export freezes selected session, guards repeat clicks and uses chosen path", async () => {
  let choose;
  const ui = mount({ save: () => new Promise(resolve => { choose = resolve; }) });
  const first = ui.exportSession();
  assert.equal(ui.exportBusy(), true);
  ui.state.currentId = "another";
  await ui.exportSession();
  choose("C:/diagnostics/session.zip");
  await first;
  assert.deepEqual(ui.calls, [["original", "C:/diagnostics/session.zip"]]);
  assert.equal(ui.exportBusy(), false);
  assert.equal(ui.notices[0][1].kind, "info");
});

test("cancel writes nothing; export failure releases busy state and never claims success", async () => {
  const cancelled = mount({ save: async () => null });
  await cancelled.exportSession();
  assert.deepEqual(cancelled.calls, []);
  assert.deepEqual(cancelled.notices, []);
  assert.equal(cancelled.exportBusy(), false);
  const failed = mount({ invoke: async () => { throw new Error("disk full"); } });
  await failed.exportSession();
  assert.equal(failed.exportBusy(), false);
  assert.match(failed.notices[0][0], /disk full/);
  assert.equal(failed.notices[0][1].kind, "error");
});

test("export sits beside Fork and compact labels preserve accessible names", () => {
  assert.ok(chat.indexOf("onClick={startFork}") < chat.indexOf("onClick={() => void exportSession()}"));
  assert.match(chat, /<span class="chat-action-label">Fork<\/span>/);
  assert.match(chat, /<span class="chat-action-label">Flow<\/span>/);
  assert.match(chat, /aria-label="线索与 Flow 分享"/);
  assert.match(chat, /aria-label=\{exportBusy\(\)/);
  const css = readFileSync(new URL("../src/app.css", import.meta.url), "utf8");
  assert.match(css, /container-name: chat-titlebar/);
  assert.match(css, /@container chat-titlebar \(max-width: 680px\)\s*\{\s*\.chat-action-label \{ display: none; \}/);
  const ipc = readFileSync(new URL("../src/ipc.ts", import.meta.url), "utf8");
  assert.match(ipc, /invoke<void>\("export_thread_diagnostics", \{ threadId, path \}\)/);
});
