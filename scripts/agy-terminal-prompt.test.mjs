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

test("the worldline defaults to two thirds of the workspace and persists changes", () => {
  const { loadAgySplitRatio, saveAgySplitRatio } = loadModule(new Map());
  assert.equal(loadAgySplitRatio(), 2 / 3);
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

test("encodeSoftMultilineTerminalChunks uses atomic ESC+CR soft newlines and preserves UTF-8 boundaries", () => {
  const { encodeSoftMultilineTerminalChunks } = loadModule(new Map());
  assert.deepEqual(encodeSoftMultilineTerminalChunks(""), []);

  const payload = "what's this\r\n\r\n附件文件：\n- C:\\Users\\油彬\\image.png";
  const chunks = encodeSoftMultilineTerminalChunks(payload, 6);
  assert.ok(chunks.length > 1);

  const decoder = new TextDecoder("utf-8", { fatal: true });
  const merged = [];
  let softNewlineChunks = 0;
  for (const chunk of chunks) {
    assert.ok(chunk.length <= 6);
    if (chunk.length === 2 && chunk[0] === 0x1b && chunk[1] === 0x0d) {
      softNewlineChunks += 1;
    } else {
      assert.notEqual(chunk[chunk.length - 1], 0x1b);
    }
    decoder.decode(chunk);
    for (const b of chunk) merged.push(b);
  }
  assert.equal(softNewlineChunks, 3);

  for (let i = 0; i < merged.length; i++) {
    assert.notEqual(merged[i], 0x0a, "no raw LF should be emitted");
    if (merged[i] === 0x0d) {
      assert.ok(i > 0 && merged[i - 1] === 0x1b, "every CR must be preceded by ESC");
    }
  }

  const decoded = new TextDecoder().decode(Uint8Array.from(merged));
  assert.equal(
    decoded,
    "what's this\x1b\r\x1b\r附件文件：\x1b\r- C:\\Users\\油彬\\image.png",
  );
});

function summarizeSteps(steps) {
  return steps.map((step) => {
    if (step.kind === "text") {
      return { kind: "text", text: step.text, delayAfterMs: step.delayAfterMs };
    }
    return {
      kind: "bytes",
      bytes: Array.from(step.bytes),
      delayAfterMs: step.delayAfterMs,
    };
  });
}

test("buildAgyAskKeySteps encodes single-select, multi-select, write-in, >9 options, and skip", () => {
  const { buildAgyAskKeySteps, buildAgyAskSkipSteps } = loadModule(new Map());

  // Single-select 1..9 uses direct Choice key without extra Enter.
  const singleFast = buildAgyAskKeySteps(
    [{ options: ["Alpha", "Beta"], isMultiSelect: false }],
    [{ selectedIndices: [1], customText: "" }],
  );
  assert.deepEqual(summarizeSteps(singleFast), [
    { kind: "bytes", bytes: [0x32], delayAfterMs: 85 },
  ]);

  // Single-select >=10 uses Down arrows + Enter.
  const tenOptions = Array.from({ length: 11 }, (_, i) => `Opt ${i + 1}`);
  const singleOverflow = buildAgyAskKeySteps(
    [{ options: tenOptions, isMultiSelect: false }],
    [{ selectedIndices: [9], customText: "" }],
  );
  assert.deepEqual(summarizeSteps(singleOverflow), [
    ...Array.from({ length: 9 }, () => ({ kind: "bytes", bytes: [0x1b, 0x5b, 0x42], delayAfterMs: 25 })),
    { kind: "bytes", bytes: [0x0d], delayAfterMs: 85 },
  ]);

  // Single-select write-in (<9 options): digit(optionCount+1), text, Enter.
  const singleCustomSmall = buildAgyAskKeySteps(
    [{ options: ["A", "B"], isMultiSelect: false }],
    [{ selectedIndices: [], customText: "custom\nline" }],
  );
  assert.deepEqual(summarizeSteps(singleCustomSmall), [
    { kind: "bytes", bytes: [0x33], delayAfterMs: 70 },
    { kind: "text", text: "custom\nline", delayAfterMs: 80 },
    { kind: "bytes", bytes: [0x0d], delayAfterMs: 90 },
  ]);

  // Single-select write-in (>=9 options): optionCount Downs + Enter, text, Enter.
  const nineOptions = Array.from({ length: 9 }, (_, i) => `Opt ${i + 1}`);
  const singleCustomLarge = buildAgyAskKeySteps(
    [{ options: nineOptions, isMultiSelect: false }],
    [{ selectedIndices: [], customText: "other" }],
  );
  assert.deepEqual(summarizeSteps(singleCustomLarge), [
    ...Array.from({ length: 9 }, () => ({ kind: "bytes", bytes: [0x1b, 0x5b, 0x42], delayAfterMs: 25 })),
    { kind: "bytes", bytes: [0x0d], delayAfterMs: 70 },
    { kind: "text", text: "other", delayAfterMs: 80 },
    { kind: "bytes", bytes: [0x0d], delayAfterMs: 90 },
  ]);

  // Multi-select (<9): toggles selected indices in order then Enter.
  const multiChoices = buildAgyAskKeySteps(
    [{ options: ["X", "Y", "Z"], isMultiSelect: true }],
    [{ selectedIndices: [2, 0], customText: "" }],
  );
  assert.deepEqual(summarizeSteps(multiChoices), [
    { kind: "bytes", bytes: [0x31], delayAfterMs: 45 },
    { kind: "bytes", bytes: [0x33], delayAfterMs: 45 },
    { kind: "bytes", bytes: [0x0d], delayAfterMs: 90 },
  ]);

  // Multi-select with >=10 options + write-in: uses Space for >=9 and Down+Enter to reach write-in.
  const multiOverflowCustom = buildAgyAskKeySteps(
    [{ options: tenOptions, isMultiSelect: true }],
    [{ selectedIndices: [1, 9], customText: "extra" }],
  );
  assert.deepEqual(summarizeSteps(multiOverflowCustom), [
    { kind: "bytes", bytes: [0x32], delayAfterMs: 45 },
    ...Array.from({ length: 8 }, () => ({ kind: "bytes", bytes: [0x1b, 0x5b, 0x42], delayAfterMs: 25 })),
    { kind: "bytes", bytes: [0x20], delayAfterMs: 45 },
    // Move from cursor=9 to write-in at index 11 (2 Downs), Enter, text, Enter
    { kind: "bytes", bytes: [0x1b, 0x5b, 0x42], delayAfterMs: 25 },
    { kind: "bytes", bytes: [0x1b, 0x5b, 0x42], delayAfterMs: 25 },
    { kind: "bytes", bytes: [0x0d], delayAfterMs: 70 },
    { kind: "text", text: "extra", delayAfterMs: 80 },
    { kind: "bytes", bytes: [0x0d], delayAfterMs: 90 },
  ]);

  // Mixed multi-question (single + skipped + multi with custom).
  const mixed = buildAgyAskKeySteps(
    [
      { options: ["Yes", "No"], isMultiSelect: false },
      { options: ["P1", "P2"], isMultiSelect: false },
      { options: ["F1", "F2"], isMultiSelect: true },
    ],
    [
      { selectedIndices: [0], customText: "" },
      { selectedIndices: [], customText: "", skipped: true },
      { selectedIndices: [1], customText: "more" },
    ],
  );
  assert.deepEqual(summarizeSteps(mixed), [
    { kind: "bytes", bytes: [0x31], delayAfterMs: 85 },
    { kind: "bytes", bytes: [0x1b], delayAfterMs: 90 },
    { kind: "bytes", bytes: [0x32], delayAfterMs: 45 },
    { kind: "bytes", bytes: [0x33], delayAfterMs: 70 },
    { kind: "text", text: "more", delayAfterMs: 80 },
    { kind: "bytes", bytes: [0x0d], delayAfterMs: 90 },
  ]);

  // Skip steps use ESC with >= 80ms delay between steps.
  const skips = buildAgyAskSkipSteps(2);
  assert.deepEqual(summarizeSteps(skips), [
    { kind: "bytes", bytes: [0x1b], delayAfterMs: 90 },
    { kind: "bytes", bytes: [0x1b], delayAfterMs: 90 },
  ]);
  for (const s of skips) {
    assert.ok((s.delayAfterMs ?? 0) >= 80);
  }
});

test("findActivePendingAgyAsk only returns pending ask on the last unresolved turn", () => {
  const { findActivePendingAgyAsk } = loadModule(new Map());
  assert.equal(findActivePendingAgyAsk([]), null);

  const pendingAsk = {
    id: "2",
    stepIndex: 2,
    questions: [{ question: "Q", options: ["A"], isMultiSelect: false }],
    status: "pending",
  };
  const answeredAsk = { ...pendingAsk, id: "1", stepIndex: 1, status: "answered" };

  // Old turn pending ask is ignored when a newer turn exists.
  assert.equal(
    findActivePendingAgyAsk([
      { index: 1, prompt: "u1", promptTimestamp: 1, asks: [pendingAsk], response: null },
      { index: 2, prompt: "u2", promptTimestamp: 2, asks: [], response: null },
    ]),
    null,
  );

  // Last turn with a final response returns null even if asks array had stale pending.
  assert.equal(
    findActivePendingAgyAsk([
      { index: 1, prompt: "u1", promptTimestamp: 1, asks: [pendingAsk], response: "done" },
    ]),
    null,
  );

  // Last turn without response returns the latest pending ask.
  assert.deepEqual(
    findActivePendingAgyAsk([
      { index: 1, prompt: "u1", promptTimestamp: 1, asks: [answeredAsk, pendingAsk], response: null },
    ]),
    pendingAsk,
  );
});
