import assert from "node:assert/strict";
import { test } from "node:test";
import { taskChains } from "../src/taskChains.ts";

const threads = [
  { id: "root" },
  { id: "stage2", parentThreadId: "root" },
  { id: "plain" },
];
const run = (over = {}) =>
  taskChains({
    threads,
    isRunning: () => false,
    advancingRoots: [],
    unfinishedRoots: [],
    queuedThreads: [],
    ...over,
  });

test("工作流没走到终点：整条链仍算未结束，即使一个会话都不 running", () => {
  const r = run({ unfinishedRoots: ["root"] });
  assert.deepEqual([...r.active].sort(), ["root", "stage2"]);
  assert.ok(!r.active.has("plain"));
  // 暂停待补充/等待审核是「等人」，不算运行中任务。
  assert.equal(r.rootCount, 0);
});

test("阶段接力空档：链仍算在跑（refreshThreads 不得冲掉忙碌态）", () => {
  const r = run({ advancingRoots: ["root"], unfinishedRoots: ["root"] });
  assert.equal(r.rootCount, 1);
  assert.deepEqual([...r.busy].sort(), ["root", "stage2"]);
});

test("普通会话按回合忙碌态计入；结束即退出", () => {
  const running = (id) => id === "plain";
  assert.deepEqual([...run({ isRunning: running }).active], ["plain"]);
  assert.equal(run({ isRunning: running }).rootCount, 1);
  assert.equal(run().active.size, 0);
});

test("busy 只认工作流推进态：假忙碌的普通会话能被后端快照自愈", () => {
  const r = run({ isRunning: (id) => id === "root" });
  assert.equal(r.busy.size, 0);
});

test("提示词队列待发：接力空档仍算未结束，但不算在跑、不锁忙碌态", () => {
  const r = run({ queuedThreads: ["plain"] });
  assert.deepEqual([...r.active], ["plain"]);
  assert.equal(r.rootCount, 0);
  assert.equal(r.busy.size, 0);
  // 队列条目在子会话上时，整条链都算未结束。
  const chained = run({ queuedThreads: ["stage2"] });
  assert.deepEqual([...chained.active].sort(), ["root", "stage2"]);
  // 队列清空即结束。
  assert.equal(run().active.size, 0);
});

test("parentThreadId 成环或指向链外会话时不死循环", () => {
  const cyclic = [
    { id: "a", parentThreadId: "b" },
    { id: "b", parentThreadId: "a" },
    { id: "c", parentThreadId: "missing" },
  ];
  const r = taskChains({
    threads: cyclic,
    isRunning: () => false,
    advancingRoots: [],
    unfinishedRoots: ["c"],
    queuedThreads: [],
  });
  // c 的父会话不在列表里 → 自己就是根；a/b 成环不会把整条链扫进空。
  assert.deepEqual([...r.active], ["c"]);
});
