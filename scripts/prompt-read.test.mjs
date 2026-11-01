import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { transformSync } from "esbuild";

const source = readFileSync(new URL("../src/store.ts", import.meta.url), "utf8");
const start = source.indexOf('listen<{ threadId: string; op?: UpdateOp; ops?: UpdateOp[] }>("acp:update"');
const end = source.indexOf('listen<{ threadId: string; cwd: string }>("thread:cwd-changed"', start);
assert.ok(start >= 0 && end > start);
const listener = transformSync(source.slice(start, end).trim().replace(/,$/, ";"), { loader: "ts" }).code;

function harness(loadingThread = false) {
  const state = { currentId: "current", loadingThread, unreadTurns: { current: 3, background: 4, other: 5 } };
  const reads = [], applied = [];
  let receive;
  const deps = {
    state,
    listen: (_, callback) => { receive = payload => callback({ payload }); },
    setUnreadTurns: (id, count) => { state.unreadTurns[id] = count; reads.push([id, count]); },
    liveUsageByThread: new Map(), trackDeltaRate() {},
    threadSnapshots: new Map([["background", {}]]), staleThreadSnapshots: new Set(),
    snapshotToolUpdates: null, flush: fn => fn(), applyOp: op => applied.push(op),
  };
  new Function(...Object.keys(deps), listener)(...Object.values(deps));
  return { state, reads, applied, receive };
}

for (const threadId of ["current", "background"]) {
  for (const batched of [false, true]) {
    test(`new prompt marks ${threadId} read (${batched ? "batch" : "single"} event)`, () => {
      const h = harness();
      const op = { t: "upsert", item: { id: 1, type: "user", text: "follow-up", images: [] } };
      h.receive({ threadId, ...(batched ? { ops: [op, { t: "usage", usage: {} }] } : { op }) });
      assert.equal(h.state.unreadTurns[threadId], 0);
      assert.equal(h.state.unreadTurns.other, 5);
      assert.deepEqual(h.reads, [[threadId, 0]]);
      assert.equal(h.applied.length, threadId === "current" ? (batched ? 2 : 1) : 0);
    });
  }
}

test("image-only prompts mark read even while the transcript snapshot is loading", () => {
  const h = harness(true);
  delete h.state.unreadTurns.current;
  h.receive({ threadId: "current", op: { t: "upsert", item: {
    id: 1, type: "user", text: "", images: [{ mimeType: "image/png", data: "YWJj" }],
  } } });
  assert.equal(h.state.unreadTurns.current, 0, "explicit zero must block stale unread snapshots");
  assert.equal(h.applied.length, 0);
});

test("assistant, tool and completion updates do not clear unread", () => {
  const h = harness();
  for (const type of ["assistant", "tool", "turn", "system"]) {
    h.receive({ threadId: "background", op: { t: "upsert", item: { id: 2, type } } });
  }
  assert.equal(h.state.unreadTurns.background, 4);
  assert.deepEqual(h.reads, []);
});
