import assert from "node:assert/strict";
import { test } from "node:test";
import { readFileSync } from "node:fs";
import { transformSync } from "esbuild";

const source = readFileSync(new URL("../src/agyTerminalPrompt.ts", import.meta.url), "utf8");
const compiled = transformSync(source, { loader: "ts", format: "cjs" }).code;

function loadModule(storageMap) {
  const mod = { exports: {} };
  const localStorage = storageMap
    ? {
        getItem(k) {
          return storageMap.has(k) ? storageMap.get(k) : null;
        },
        setItem(k, v) {
          storageMap.set(k, String(v));
        },
      }
    : undefined;
  const fn = new Function("exports", "module", "localStorage", compiled);
  fn(mod.exports, mod, localStorage);
  return mod.exports;
}

test("formatTerminalPromptWithPaths formats text and deduplicates paths", () => {
  const { formatTerminalPromptWithPaths } = loadModule(new Map());
  assert.equal(formatTerminalPromptWithPaths("  hello\r\nworld  ", []), "hello\nworld");
  assert.equal(
    formatTerminalPromptWithPaths("", ["C:/a/b.png"]),
    "请查看以下附件文件：\n- C:/a/b.png",
  );
  assert.equal(
    formatTerminalPromptWithPaths("请分析截图", [
      "C:/My Files/shot 1.png",
      "C:/My Files/shot 1.png",
      "D:\\code\\note.txt",
    ]),
    '请分析截图\n\n附件文件：\n- "C:/My Files/shot 1.png"\n- D:\\code\\note.txt',
  );
});

test("clampAgySplitRatio bounds values and falls back on non-finite", () => {
  const { clampAgySplitRatio, DEFAULT_AGY_SPLIT_RATIO } = loadModule(new Map());
  assert.equal(clampAgySplitRatio(Number.NaN), DEFAULT_AGY_SPLIT_RATIO);
  assert.equal(clampAgySplitRatio(Number.POSITIVE_INFINITY), DEFAULT_AGY_SPLIT_RATIO);
  assert.equal(clampAgySplitRatio(0.1), 0.25);
  assert.equal(clampAgySplitRatio(0.95), 0.72);
  assert.equal(clampAgySplitRatio(0.55), 0.55);
});

test("loadAgySplitOpen defaults to true and persists false", () => {
  const store = new Map();
  const { loadAgySplitOpen, saveAgySplitOpen, loadAgySplitRatio, saveAgySplitRatio } = loadModule(store);
  assert.equal(loadAgySplitOpen(), true);
  saveAgySplitOpen(false);
  assert.equal(loadAgySplitOpen(), false);
  saveAgySplitOpen(true);
  assert.equal(loadAgySplitOpen(), true);

  assert.equal(loadAgySplitRatio(), 0.5);
  saveAgySplitRatio(0.62);
  assert.equal(loadAgySplitRatio(), 0.62);
});

test("reconcileOptimisticTurns and mergeAgyTurnsWithOptimistic keep optimistic user turns immediate and deduplicated", () => {
  const { reconcileOptimisticTurns, mergeAgyTurnsWithOptimistic } = loadModule(new Map());
  const turn1 = { index: 1, prompt: "first", promptTimestamp: 100, response: "ok" };
  const turn2 = { index: 2, prompt: "second", promptTimestamp: 200, response: null };

  const opt1 = { id: 1, prompt: "second", timestamp: 199, baseTurnCount: 1 };
  const opt2 = { id: 2, prompt: "third", timestamp: 299, baseTurnCount: 2 };

  // Real turns only has turn1 -> both optimistic turns remain visible in sequence.
  const mergedBefore = mergeAgyTurnsWithOptimistic([turn1], [opt1, opt2]);
  assert.equal(mergedBefore.length, 3);
  assert.equal(mergedBefore[1].index, 2);
  assert.equal(mergedBefore[1].prompt, "second");
  assert.equal(mergedBefore[2].index, 3);
  assert.equal(mergedBefore[2].prompt, "third");

  // Once real turns includes turn2, opt1 is reconciled out while opt2 stays.
  assert.deepEqual(reconcileOptimisticTurns([turn1, turn2], [opt1, opt2]), [opt2]);
  const mergedMid = mergeAgyTurnsWithOptimistic([turn1, turn2], [opt1, opt2]);
  assert.equal(mergedMid.length, 3);
  assert.equal(mergedMid[1].prompt, "second");
  assert.equal(mergedMid[2].index, 3);
  assert.equal(mergedMid[2].prompt, "third");

  // Once turn3 arrives, optimistic list drains cleanly.
  const turn3 = { index: 3, prompt: "third", promptTimestamp: 300, response: "done" };
  assert.deepEqual(reconcileOptimisticTurns([turn1, turn2, turn3], [opt1, opt2]), []);
  assert.equal(mergeAgyTurnsWithOptimistic([turn1, turn2, turn3], [opt1, opt2]).length, 3);
});
