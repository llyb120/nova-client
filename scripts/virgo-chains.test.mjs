import assert from "node:assert/strict";
import { test } from "node:test";
import { virgoChains } from "../src/virgoChains.ts";

const threads = [
  { id: "root" },
  { id: "stage2", parentThreadId: "root" },
  { id: "plain" },
];
const run = (over = {}) =>
  virgoChains({
    threads,
    isRunning: () => false,
    advancingRoots: [],
    unfinishedRoots: [],
    ...over,
  });

test("工作流没走到终点：整条链留在室女座，即使一个会话都不 running", () => {
  const r = run({ unfinishedRoots: ["root"] });
  assert.deepEqual([...r.hidden].sort(), ["root", "stage2"]);
  assert.ok(!r.hidden.has("plain"));
  // 暂停待补充/等待审核是「等人」，不算运行中任务。
  assert.equal(r.rootCount, 0);
});

test("阶段接力空档：链仍在室女座且算在跑（refreshThreads 不得冲掉忙碌态）", () => {
  const r = run({ advancingRoots: ["root"], unfinishedRoots: ["root"] });
  assert.equal(r.rootCount, 1);
  assert.deepEqual([...r.busy].sort(), ["root", "stage2"]);
});

test("普通会话按回合忙碌态归室女座；结束即回到普通列表", () => {
  const running = (id) => id === "plain";
  assert.deepEqual([...run({ isRunning: running }).hidden], ["plain"]);
  assert.equal(run({ isRunning: running }).rootCount, 1);
  assert.equal(run().hidden.size, 0);
});

test("busy 只认工作流推进态：假忙碌的普通会话能被后端快照自愈", () => {
  const r = run({ isRunning: (id) => id === "root" });
  assert.equal(r.busy.size, 0);
});

test("parentThreadId 成环或指向链外会话时不死循环", () => {
  const cyclic = [
    { id: "a", parentThreadId: "b" },
    { id: "b", parentThreadId: "a" },
    { id: "c", parentThreadId: "missing" },
  ];
  const r = virgoChains({
    threads: cyclic,
    isRunning: () => false,
    advancingRoots: [],
    unfinishedRoots: ["c"],
  });
  // c 的父会话不在列表里 → 自己就是根；a/b 成环不会把整条链扫进空。
  assert.deepEqual([...r.hidden], ["c"]);
});
