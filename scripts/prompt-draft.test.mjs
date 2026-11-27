import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { transformSync } from "esbuild";
import { createEffect, createRoot, createSignal, flush, onCleanup, untrack } from "@solidjs/signals";

const read = (path) => readFileSync(new URL(path, import.meta.url), "utf8");
const module = { exports: {} };
new Function("module", "exports", transformSync(read("../src/promptDraft.ts"), {
  loader: "ts", format: "cjs",
}).code)(module, module.exports);
const drafts = module.exports;

test("session drafts preserve exact text and attachments independently, and empty input clears them", () => {
  const image = { name: "cat.png", mimeType: "image/png", data: "abc" };
  drafts.saveSessionDraft("a", "  多行\n草稿  ", [image]);
  drafts.saveSessionDraft("b", "B", []);
  drafts.saveSessionDraft(null, "首页", []);
  image.name = "changed";
  assert.deepEqual(drafts.takeSessionDraft("a"), {
    text: "  多行\n草稿  ", images: [{ ...image, name: "cat.png" }],
  });
  assert.equal(drafts.takeSessionDraft("a"), null);
  assert.equal(drafts.takeSessionDraft("b").text, "B");
  assert.equal(drafts.takeSessionDraft(null).text, "首页");
  drafts.saveSessionDraft("a", "old", []);
  drafts.saveSessionDraft("a", "", []);
  assert.equal(drafts.takeSessionDraft("a"), null);
  drafts.saveSessionDraft("a", "", [image]);
  assert.equal(drafts.takeSessionDraft("a").images.length, 1);
});

test("opening a terminal preset seeds its draft before navigating without changing the login configuration", async () => {
  const source = read("../src/store.ts");
  const action = source.slice(source.indexOf("export async function createTerminalThread("), source.indexOf("/** 待创建会话的占位"));
  const createTerminalThread = new Function("deps", `
    const { api, saveSessionDraft, terminalDraftKey, rememberThreadSnapshot, flush, setState, reportActivity, refreshThreads, refreshProjects } = deps;
    ${transformSync(action.replace("export ", ""), { loader: "ts" }).code}
    return createTerminalThread;
  `);
  const state = { threads: [], running: {} };
  const calls = [];
  const noop = () => {};
  const open = createTerminalThread({
    ...drafts, rememberThreadSnapshot: noop, reportActivity: noop, refreshThreads: noop, refreshProjects: noop,
    flush: (action) => action(),
    setState: (action) => {
      action(state);
      assert.ok(drafts.hasTerminalDraft(state.currentId), "draft must be installed before the view switches");
    },
    api: {
      createTerminalThread: async (cwd, title, config) => {
        calls.push({ cwd, title, config });
        return { id: `term-${calls.length}`, title, cwd, agentKind: "lyra", terminalConfig: config };
      },
    },
  });
  const image = { name: "note.txt", mimeType: "text/plain", data: "eA==" };
  const draft = { text: "  第一行\n第二行  ", images: [image] };
  const shell = { id: "shell", name: " Shell ", command: "start cli", shell: " pwsh ", args: ["-NoProfile"] };
  const first = await open("D:/project", shell, draft);
  assert.deepEqual(calls[0], { cwd: "D:/project", title: "Shell", config: {
    mode: "command", bypassPermissions: true, initCommand: "start cli", shell: "pwsh", args: ["-NoProfile"],
  } });
  assert.deepEqual(drafts.takeSessionDraft(drafts.terminalDraftKey(first)), draft);
  const second = await open("D:/other", { ...shell, id: "agy", mode: "antigravity", agyAccountId: "saved-account", model: " model-x ", bypassPermissions: false }, draft);
  assert.deepEqual(calls[1].config, {
    mode: "antigravity", bypassPermissions: false, model: "model-x", agyAccountId: "saved-account",
    initCommand: "", shell: "", args: [],
  });
  const key = drafts.terminalDraftKey(second);
  assert.deepEqual(drafts.takeSessionDraft(key), { ...draft, autoSend: true });
  assert.equal(drafts.takeSessionDraft(key), null, "automatic send intent can only be consumed once");
});

test("terminal sends wait for text writes before committing and don't submit a failed write", async () => {
  const source = read("../src/terminalSessions.ts");
  const start = source.indexOf("export async function sendTerminalInput(");
  const end = source.indexOf("/** 向运行中的终端发送 Esc", start);
  const send = new Function("window", "encodeSoftMultilineTerminalChunks", `
    ${transformSync(source.slice(start, end).replace("export ", ""), { loader: "ts" }).code}
    return sendTerminalInput;
  `)({ setTimeout: (callback) => queueMicrotask(callback) }, (text) => [new TextEncoder().encode(text)]);
  const written = [];
  let error = "";
  let fail = false;
  let release;
  const pending = new Promise((resolve) => { release = resolve; });
  const tab = {
    status: () => "running", error: () => error, terminal: { options: {}, modes: {} }, input: pending,
    sendBytes(data) {
      this.input = this.input.then(() => {
        if (fail) error = "write failed";
        else written.push(new TextDecoder().decode(data));
      });
    },
  };
  const good = send(tab, "hello", { submit: true });
  await Promise.resolve();
  assert.deepEqual(written, []);
  release();
  assert.equal(await good, true);
  assert.deepEqual(written, ["hello", "\r"]);
  fail = true;
  assert.equal(await send(tab, "failed prompt", { submit: true }), false);
  assert.deepEqual(written, ["hello", "\r"]);
});

test("a forwarded AGY draft waits for CLI readiness, attempts only once and survives a failed send", async () => {
  const source = read("../src/components/TerminalComposer.tsx");
  const section = (begin, end) => source.slice(source.indexOf(begin), source.indexOf(end, source.indexOf(begin)));
  const body = [
    section("  let draftThreadId", "  const flushInputResize"),
    section("  const empty", "  const insertTextAtCursor"),
    section("  const clearInput", "  const onKeyDown"),
  ].join("\n");
  const mount = new Function("props", "deps", `
    const { createSignal, createEffect, untrack, onSettled, window, terminalDraftKey, takeSessionDraft,
      saveSessionDraft, createImageAttachments, formatTerminalPromptWithPaths, showToast } = deps;
    ${transformSync(body, { loader: "ts" }).code}
    return { text, submit };
  `);
  const [autoState, setAutoState] = createSignal("waiting");
  const attempts = [];
  const settle = [];
  const key = drafts.terminalDraftKey("agy-ready");
  drafts.saveSessionDraft(key, "first prompt", [], true);
  const view = createRoot((dispose) => ({ dispose, ...mount({
    threadId: "agy-ready", agy: true, terminalReady: true,
    get autoSendState() { return autoState(); },
    onSend: async (payload, submit) => { attempts.push([payload, submit]); return attempts.length > 1; },
  }, {
    ...drafts, createSignal, createEffect, untrack,
    onSettled: (fn) => settle.push(fn),
    window: { setTimeout: () => 1, clearTimeout: () => {} },
    createImageAttachments: () => {
      const [images, set] = createSignal([]);
      return { images, set, clear: () => set([]) };
    },
    formatTerminalPromptWithPaths: (text) => text,
    showToast: () => {},
  }) }));
  settle.forEach((fn) => fn()); flush();
  assert.deepEqual(attempts, [], "PTY running is not enough to submit a prompt");
  setAutoState("ready"); flush();
  await Promise.resolve(); flush();
  assert.deepEqual(attempts, [["first prompt", true]]);
  assert.equal(view.text(), "first prompt");
  setAutoState("waiting"); flush(); setAutoState("ready"); flush();
  assert.equal(attempts.length, 1, "do not automatically retry an uncertain send");
  await view.submit(true); flush();
  assert.equal(view.text(), "");
  view.dispose();
});

test("Composer restores on switch and remount, clears sent drafts, and saves under the departing session", () => {
  const source = read("../src/components/Composer.tsx");
  const init = source.slice(source.indexOf("  let draftThreadId"), source.indexOf("  const [cursor"));
  const start = source.lastIndexOf("  createEffect(", source.indexOf("saveSessionDraft(draftThreadId"));
  const lifecycle = source.slice(start, source.indexOf("  const currentQueuedPrompts"));
  const mount = new Function("state", "api", `
    const { createEffect, createSignal, untrack, onCleanup, saveSessionDraft, takeSessionDraft, rememberPromptDraft } = api;
    ${init}
    const [images, set] = createSignal(initialDraft?.images ?? []);
    const attach = { images, set };
    const setCursor = () => {}, setSlashStart = () => {}, setHistoryOpen = () => {};
    ${lifecycle}
    return { text, setText, attach };
  `);
  const [currentId, setCurrentId] = createSignal("first");
  const state = { get currentId() { return currentId(); } };
  const api = { ...drafts, createEffect, createSignal, untrack, onCleanup };
  let composer, dispose;
  createRoot((cleanup) => { dispose = cleanup; composer = mount(state, api); });
  flush();
  composer.setText("first draft");
  composer.attach.set([{ name: "file.txt", mimeType: "text/plain", data: "xyz" }]);
  setCurrentId("second"); flush();
  assert.equal(composer.text(), "");
  composer.setText("second draft");
  setCurrentId("first"); flush();
  assert.equal(composer.text(), "first draft");
  assert.equal(composer.attach.images()[0].name, "file.txt");
  composer.setText("");
  composer.attach.set([]);
  setCurrentId("second"); flush();
  assert.equal(composer.text(), "second draft");
  setCurrentId("first"); flush();
  assert.equal(composer.text(), "");
  composer.setText("survives unmount");
  setCurrentId(null); flush();
  dispose();
  assert.equal(drafts.takeSessionDraft(null), null);
  setCurrentId("first"); flush();
  createRoot((cleanup) => { dispose = cleanup; composer = mount(state, api); });
  assert.equal(composer.text(), "survives unmount");
  dispose();
});
