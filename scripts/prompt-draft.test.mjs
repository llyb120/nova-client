import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { transformSync } from "esbuild";
import { createEffect, createRoot, createSignal, on, onCleanup } from "solid-js/dist/solid.js";

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

test("Composer restores on switch and remount, clears sent drafts, and saves under the departing session", () => {
  const source = read("../src/components/Composer.tsx");
  const init = source.slice(source.indexOf("  let draftThreadId"), source.indexOf("  const [cursor"));
  const start = source.lastIndexOf("  createEffect(", source.indexOf("saveSessionDraft(draftThreadId"));
  const lifecycle = source.slice(start, source.indexOf("  const currentQueuedPrompts"));
  const mount = new Function("state", "api", `
    const { createEffect, createSignal, on, onCleanup, saveSessionDraft, takeSessionDraft, rememberPromptDraft } = api;
    ${init}
    const [images, set] = createSignal(initialDraft?.images ?? []);
    const attach = { images, set };
    const setCursor = () => {}, setSlashStart = () => {}, setHistoryOpen = () => {};
    ${lifecycle}
    return { text, setText, attach };
  `);
  const [currentId, setCurrentId] = createSignal("first");
  const state = { get currentId() { return currentId(); } };
  const api = { ...drafts, createEffect, createSignal, on, onCleanup };
  let composer, dispose;
  createRoot((cleanup) => { dispose = cleanup; composer = mount(state, api); });
  composer.setText("first draft");
  composer.attach.set([{ name: "file.txt", mimeType: "text/plain", data: "xyz" }]);
  setCurrentId("second");
  assert.equal(composer.text(), "");
  composer.setText("second draft");
  setCurrentId("first");
  assert.equal(composer.text(), "first draft");
  assert.equal(composer.attach.images()[0].name, "file.txt");
  composer.setText("");
  composer.attach.set([]);
  setCurrentId("second");
  assert.equal(composer.text(), "second draft");
  setCurrentId("first");
  assert.equal(composer.text(), "");
  composer.setText("survives unmount");
  setCurrentId(null);
  dispose();
  assert.equal(drafts.takeSessionDraft(null), null);
  setCurrentId("first");
  createRoot((cleanup) => { dispose = cleanup; composer = mount(state, api); });
  assert.equal(composer.text(), "survives unmount");
  dispose();
});
