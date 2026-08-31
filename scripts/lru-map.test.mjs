import assert from "node:assert/strict";
import { test } from "node:test";
import { LruMap } from "../src/lruMap.ts";

test("LruMap bounds entries, refreshes reads, and preserves the active key", () => {
  const cache = new LruMap(3);
  cache.set("active", 1);
  cache.set("old", 2);
  cache.set("recent", 3);
  assert.equal(cache.get("old"), 2);

  assert.deepEqual(cache.set("next", 4, "active"), ["recent"]);
  assert.deepEqual([...cache.keys()], ["old", "next", "active"]);
  assert.equal(cache.peek("active"), 1);
  assert.equal(cache.peek("recent"), undefined);
});
