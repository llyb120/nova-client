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

for (const cached of [false, true]) {
  test(`${cached ? "暖" : "冷"}切换先接通推流，快照不能吞掉工具完成/失败状态`, async () => {
    const active = deferred();
    const snapshot = deferred();
    const calls = [];
    const running = { id: 6, type: "tool", status: "in_progress", ts: 1000 };
    const thread = { id: "target", items: [running], agentKind: "codex" };
    const state = { currentId: "old", unreadTurns: {}, running: { target: true }, threads: [], items: [] };
    const applyOp = (op) => {
      const index = state.items.findIndex((item) => item.id === op.item.id);
      if (index < 0) state.items.push(op.item);
      else state.items[index] = op.item;
    };
    const showThreadSnapshot = (value, loadingThread) => Object.assign(state, structuredClone(value), {
      currentId: value.id, loadingThread,
    });
    const dependencies = {
      state, applyOp, showThreadSnapshot,
      api: {
        reportActivity: () => { calls.push("activate"); return active.promise; },
        getThread: () => { calls.push("snapshot"); return snapshot.promise; },
      },
      getThreadSnapshot: () => cached ? structuredClone(thread) : undefined,
      setState: (value) => Object.assign(state, value),
      batch: (fn) => fn(),
      threadSnapshots: new Map(), staleThreadSnapshots: new Set(["target"]), liveUsageByThread: new Map(),
      unhideVirgoThread() {}, setUnreadTurns() {}, markThreadSwitchStart() {},
      flushPendingStreamUpdates() {}, rememberCurrentThreadSnapshot() {},
      discardPendingStreamUpdates() {}, resetExpanded() {}, traceThreadSwitch() {},
      rememberThreadSnapshot() {}, ensurePeerModels() {}, ensureModelOptions() {},
    };
    const { openThread, receive } = new Function(...Object.keys(dependencies), `
      let openThreadRequest = 0, snapshotToolUpdates, switchTraceStart = 0, lastActivityReport = 0;
      ${js(section("export async function openThread(", "export function closeThread(")).replace("export ", "")}
      return { openThread, receive(op) {
        const e = { payload: { threadId: "target" } };
        ${js(section("    const apply = (op: UpdateOp) => {", "    if (ops.length > 1)"))}
        apply(op);
      } };
    `)(...Object.values(dependencies));

    const opened = openThread("target");
    assert.deepEqual(calls, ["activate"]);
    active.resolve();
    await Promise.resolve();
    assert.deepEqual(calls, ["activate", "snapshot"]);
    for (const [id, status] of [[6, "completed"], [7, "failed"]]) {
      receive({ t: "upsert", item: { ...running, id, status, rawOutput: { durationMs: 15535 } } });
    }
    snapshot.resolve(structuredClone(thread)); // older in_progress snapshot arrives last
    await opened;
    assert.equal(state.loadingThread, false);
    assert.deepEqual(state.items.map((item) => item.status), ["completed", "failed"]);
    assert.equal(state.items[0].rawOutput.durationMs, 15535);
  });
}
