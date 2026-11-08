import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { transformSync } from "esbuild";

const source = readFileSync(new URL("../src/store.ts", import.meta.url), "utf8");
const section = (start, end) => source.slice(source.indexOf(start), source.indexOf(end, source.indexOf(start)));
const js = (text) => transformSync(text, { loader: "ts", target: "esnext" }).code;
const deferred = () => {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
};

const stale = [
  { id: 1, type: "user", text: "跑个命令", ts: 900 },
  { id: 6, type: "tool", status: "in_progress", ts: 1000 },
];
const truth = [
  { id: 1, type: "user", text: "跑个命令", ts: 900 },
  { id: 6, type: "tool", status: "completed", ts: 1000, rawOutput: { durationMs: 8314 } },
  { id: 7, type: "assistant", text: "结论", ts: 2000 },
];

function harness(state) {
  const snapshot = deferred();
  const calls = { fetched: 0, shown: 0, discarded: 0, remembered: 0 };
  const api = { getThreadView: () => { calls.fetched++; return snapshot.promise; } };
  const showThreadSnapshot = (value, loadingThread) => {
    calls.shown++;
    for (const item of value.items) {
      const index = state.items.findIndex((current) => current.id === item.id);
      if (index < 0) state.items.push(item);
      else state.items[index] = item;
    }
    state.loadingThread = loadingThread;
  };
  const { resyncFinishedTurn, bumpRequest, noteStreamSeq } = new Function(
    "state", "api", "discardPendingStreamUpdates", "rememberThreadSnapshot", "showThreadSnapshot", "window", "setState",
    `let openThreadRequest = 0;
     ${js(section("async function resyncFinishedTurn(", "let initialized = false;"))}
     return { resyncFinishedTurn, noteStreamSeq, bumpRequest() { openThreadRequest++; } };`,
  )(
    state, api,
    () => { calls.discarded++; },
    () => { calls.remembered++; },
    showThreadSnapshot,
    { setTimeout: (fn, ms) => setTimeout(fn, ms) },
    (update) => update(state),
  );
  return { resyncFinishedTurn, noteStreamSeq, bumpRequest, snapshot, calls };
}

const idleState = () => ({ currentId: "t", loadingThread: false, running: { t: false }, items: structuredClone(stale), logs: [] });

test("收尾补齐丢失的工具完成与结论", async () => {
  const state = idleState();
  const { resyncFinishedTurn, snapshot, calls } = harness(state);
  const done = resyncFinishedTurn("t");
  snapshot.resolve({ id: "t", items: structuredClone(truth), agentKind: "lyra" });
  await done;
  assert.equal(calls.fetched, 1);
  assert.equal(calls.shown, 1);
  assert.equal(calls.discarded, 1);
  assert.equal(calls.remembered, 1);
  assert.deepEqual(state.items.find((item) => item.id === 6).rawOutput, { durationMs: 8314 });
  assert.equal(state.items.find((item) => item.id === 7).text, "结论");
});

test("已切走、已开新轮次或正在加载时不覆盖当前界面", async () => {
  for (const mutate of [
    (state, done) => { state.currentId = "other"; done.bumpRequest(); },
    (state) => { state.running.t = true; },
    (state, done) => { done.bumpRequest(); },
  ]) {
    const state = idleState();
    const { resyncFinishedTurn, snapshot, calls, bumpRequest } = harness(state);
    const done = resyncFinishedTurn("t");
    mutate(state, { bumpRequest });
    snapshot.resolve({ id: "t", items: structuredClone(truth), agentKind: "lyra" });
    await done;
    assert.equal(calls.shown, 0);
    assert.equal(calls.discarded, 0);
    assert.equal(state.items.length, stale.length);
  }
  const loading = { ...idleState(), loadingThread: true };
  const { resyncFinishedTurn, calls } = harness(loading);
  await resyncFinishedTurn("t");
  assert.equal(calls.fetched, 0);
  assert.equal(calls.shown, 0);
});

test("推流序号跳号：空闲会话立即补快照，运行中留给收尾校准", async () => {
  const state = idleState();
  const { noteStreamSeq, snapshot, calls } = harness(state);
  noteStreamSeq("t", 1);
  noteStreamSeq("t", 2);
  await new Promise((resolve) => setTimeout(resolve, 350));
  assert.equal(calls.fetched, 0, "连续序号不该触发快照");
  noteStreamSeq("t", 5);
  await new Promise((resolve) => setTimeout(resolve, 350));
  assert.equal(calls.fetched, 1, "跳号后补一次快照");
  snapshot.resolve({ id: "t", items: structuredClone(truth), agentKind: "lyra" });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(calls.shown, 1);

  const running = { ...idleState(), running: { t: true } };
  const busy = harness(running);
  busy.noteStreamSeq("t", 1);
  busy.noteStreamSeq("t", 9);
  await new Promise((resolve) => setTimeout(resolve, 350));
  assert.equal(busy.calls.fetched, 0, "运行中不插入快照，避免覆盖更新的增量");
  busy.snapshot.resolve({ id: "t", items: structuredClone(truth), agentKind: "lyra" });
  running.running.t = false;
  await busy.resyncFinishedTurn("t");
  assert.equal(busy.calls.shown, 1, "漏过增量时收尾仍会校准");
});
