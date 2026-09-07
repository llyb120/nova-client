import assert from "node:assert/strict";
import { test } from "node:test";
import { chainGroupAnchor, latestFireStage } from "../src/threadDisplay.ts";

// 回归：三级工作流链（root → stage2 → stage3），stage2/stage3 的 cwd 被 agent
// 中途切到别的目录（change_working_directory）。分组锚点必须一路取到链根，
// 否则 stage3 与父级分进不同分组、组内找不到父节点，侧栏（含室女座）裂成两条会话。
const threads = [
  { id: "root", cwd: "D:/code/repo" },
  { id: "stage2", parentThreadId: "root", cwd: "D:/code/repo-worktree" },
  { id: "stage3", parentThreadId: "stage2", cwd: "D:/code/repo-worktree" },
  { id: "plain", cwd: "D:/code/other" },
];

test("多级 stage 链：所有环节的分组锚点都是链根", () => {
  for (const t of threads.slice(0, 3)) {
    assert.equal(chainGroupAnchor(threads, t).id, "root");
  }
  assert.equal(chainGroupAnchor(threads, threads[3]).id, "plain");
});

test("父级不在当前列表时停在自身；成环时不死循环", () => {
  const orphan = { id: "orphan", parentThreadId: "missing", cwd: "x" };
  assert.equal(chainGroupAnchor(threads, orphan).id, "orphan");
  const cyclic = [
    { id: "a", parentThreadId: "b" },
    { id: "b", parentThreadId: "a" },
  ];
  assert.ok(["a", "b"].includes(chainGroupAnchor(cyclic, cyclic[0]).id));
});

// 回归：链上较早的 stage 完成后留下未读，此时整条链仍在运行后续阶段。
// 点击链根必须直达正在运行的 stage，而不是被未读的旧 stage 劫持。
const runningChain = [
  { id: "root", title: "目标", createdAt: 0, cwd: "D:/code/repo" },
  { id: "s1", parentThreadId: "root", title: "[WF] 节点A", stageSourceThreadId: "root", createdAt: 1, cwd: "D:/code/repo" },
  { id: "s2", parentThreadId: "s1", title: "[WF] 节点B", stageSourceThreadId: "s1", createdAt: 2, cwd: "D:/code/repo" },
];
const isRunning = (id) => id === "s2";
const unreadOf = (id) => (id === "s1" ? 1 : 0);

test("链运行中：点击直达正在运行的 stage，即使旧 stage 有未读", () => {
  assert.equal(latestFireStage(runningChain, runningChain[0], isRunning, unreadOf)?.id, "s2");
});

test("链空闲时保持原行为：未读优先，其次最新 stage", () => {
  const idle = latestFireStage(runningChain, runningChain[0], () => false, unreadOf);
  assert.equal(idle?.id, "s1");
  assert.equal(latestFireStage(runningChain, runningChain[0])?.id, "s2");
});

test("打开未读快捷键（prefer unread）：链运行中仍优先未读的旧 stage", () => {
  assert.equal(
    latestFireStage(runningChain, runningChain[0], isRunning, unreadOf, "unread")?.id,
    "s1",
  );
});

// 回归：阶段接力空档里没有任何会话 running，但整条链 busy（侧栏在转圈）。
// 点击的运行判定并入 busy 后，链上每个会话都算运行中，取最新创建的即当前阶段。
test("接力空档：链 busy 但无人 running，点击直达最新阶段（未读已清）", () => {
  const busyAll = () => true;
  assert.equal(latestFireStage(runningChain, runningChain[0], busyAll, () => 0)?.id, "s2");
});

test("接力空档：链 busy 时运行口径优先于旧 stage 的未读", () => {
  const busyAll = () => true;
  assert.equal(latestFireStage(runningChain, runningChain[0], busyAll, unreadOf)?.id, "s2");
});

test("链上没有 stage 节点时保持原行为：打开被点击的会话本身", () => {
  const plain = [{ id: "a", title: "普通会话", createdAt: 1, cwd: "x" }];
  assert.equal(latestFireStage(plain, plain[0], isRunning, unreadOf), undefined);
});
