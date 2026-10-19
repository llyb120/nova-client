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
    queuedThreads: [],
    ...over,
  });

test("Fire / stage 父子会话按同一条运行链计数，普通会话独立计数", () => {
  for (const title of ["[Fire] 目标", "普通 stage 目标"]) {
    for (const running of [["root"], ["stage2"], ["root", "stage2"]]) {
      const r = run({ threads: threads.map((thread) => ({ ...thread, title })), isRunning: (id) => running.includes(id) });
      assert.deepEqual([...r.active].sort(), ["root", "stage2"]);
      assert.equal(r.rootCount, 1);
    }
  }
  const parallel = run({ isRunning: (id) => id === "stage2" || id === "plain" });
  assert.deepEqual([...parallel.active].sort(), ["plain", "root", "stage2"]);
  assert.equal(parallel.rootCount, 2);
});

test("普通会话按回合忙碌态计入；结束即退出", () => {
  const running = (id) => id === "plain";
  assert.deepEqual([...run({ isRunning: running }).active], ["plain"]);
  assert.equal(run({ isRunning: running }).rootCount, 1);
  assert.equal(run().active.size, 0);
});

test("提示词队列待发：接力空档仍算未结束，但不算在跑", () => {
  const r = run({ queuedThreads: ["plain"] });
  assert.deepEqual([...r.active], ["plain"]);
  assert.equal(r.rootCount, 0);
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
    queuedThreads: ["c"],
  });
  // c 的父会话不在列表里 → 自己就是根；a/b 成环不会把整条链扫进空。
  assert.deepEqual([...r.active], ["c"]);
});
