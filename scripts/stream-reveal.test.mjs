import assert from "node:assert/strict";
import { test } from "node:test";
import { advanceStreamText, latestStreamTextItem } from "../src/streamReveal.ts";

test("stream reveal clamps dynamic speed and carries fractional progress", () => {
  assert.deepEqual(advanceStreamText("ab", "abcdef", 0), { text: "ab", remainder: 0 });

  const slow = advanceStreamText("", "abcdef", 8);
  assert.equal(slow.text, "");
  assert(slow.remainder > 0);
  const accumulated = advanceStreamText(slow.text, "abcdef", 9, slow.remainder);
  assert.equal(accumulated.text, "a");

  assert.equal(advanceStreamText("ab", "abcdef", 220).text, "abcdef");
  assert.equal(advanceStreamText("old", "new", 16).text, "new");
  assert.equal(advanceStreamText("", "😀x", 17).text, "😀");
});

test("only the latest text item of an unfinished running group is revealable", () => {
  const old = { id: 1, type: "assistant", text: "old" };
  const latest = { id: 2, type: "assistant", text: "latest" };
  assert.equal(latestStreamTextItem([{ body: [old, latest] }], true), latest);
  assert.equal(latestStreamTextItem([{ body: [old, latest], turn: {} }], true), undefined);
  assert.equal(latestStreamTextItem([{ body: [old, latest] }], false), undefined);
  assert.equal(latestStreamTextItem([{ body: [latest, { id: 3, type: "tool" }] }], true), undefined);
});
