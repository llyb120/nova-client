import assert from "node:assert/strict";
import { test } from "node:test";
import { readFileSync } from "node:fs";
import { transformSync } from "esbuild";
import { chainGroupAnchor, latestFireStage } from "../src/threadDisplay.ts";
import { nextRunningThread } from "../src/nextRunningThread.ts";
import { isScratch, scratchParent } from "../src/utils.ts";

test("侧栏将 Windows 长路径前缀与普通源仓库路径归为同组", () => {
  const sidebar = readFileSync(new URL("../src/components/Sidebar.tsx", import.meta.url), "utf8");
  const body = sidebar.slice(sidebar.indexOf("const groupByCwd ="), sidebar.indexOf("const currentGroups ="));
  const { code } = transformSync(`${body}\nreturn groupByCwd;`, { loader: "ts" });
  const group = new Function("chainGroupAnchor", "isScratch", "scratchParent", code)(chainGroupAnchor, isScratch, scratchParent);
  for (const repo of [String.raw`D:\code\repo`, String.raw`\\server\share\repo`]) {
    const extended = repo.startsWith("\\\\") ? `\\\\?\\UNC\\${repo.slice(2)}` : `\\\\?\\${repo}`;
    const rows = [
      { id: "plain", cwd: repo, roamingRole: "guest" },
      { id: "worktree", cwd: "worktree-dir", roamingRole: "guest", worktree: { repo: extended, path: "worktree-dir" } },
      { id: "other", cwd: `${repo}-other`, roamingRole: "guest" },
    ];
    const groups = group(rows);
    assert.equal(groups.length, 2);
    assert.equal(groups[0][0], repo);
    assert.deepEqual(groups[0][1].map((thread) => thread.id), ["plain", "worktree"]);
  }
});

// 回归：三级 stage 链（root → stage2 → stage3），stage2/stage3 的 cwd 被 agent
// 中途切到别的目录（change_working_directory）。分组锚点必须一路取到链根，
// 否则 stage3 与父级分进不同分组、组内找不到父节点，侧栏裂成两条会话。
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
  { id: "s1", parentThreadId: "root", title: "检查方案", stageSourceThreadId: "root", createdAt: 1, cwd: "D:/code/repo" },
  { id: "s2", parentThreadId: "s1", title: "执行方案", stageSourceThreadId: "s1", createdAt: 2, cwd: "D:/code/repo" },
];
const isRunning = (id) => id === "s2";
const unreadOf = (id) => (id === "s1" ? 1 : 0);

test("链运行中：点击直达正在运行的 stage，即使旧 stage 有未读", () => {
  assert.equal(latestFireStage(runningChain, runningChain[0], isRunning, unreadOf)?.id, "s2");
});

test("进入会话时按实际运行节点跳转，覆盖无标记的 stage 和根阶段", () => {
  const chain = runningChain.map((thread) => ({ ...thread, title: "普通标题", stageSourceThreadId: undefined }));
  assert.equal(latestFireStage(chain, chain[0], (id) => id === "s1")?.id, "s1");
  assert.equal(latestFireStage(runningChain, runningChain[0], (id) => id === "s1", unreadOf, "running")?.id, "s1");
  assert.equal(latestFireStage(runningChain, runningChain[0], (id) => id === "root", unreadOf, "running")?.id, "root");
});

test("Fire 标题仍能识别阶段，不要求普通 stage 的来源标记", () => {
  const chain = runningChain.map((thread) => ({ ...thread, title: `[Fire] ${thread.title}`, stageSourceThreadId: undefined }));
  assert.equal(latestFireStage(chain, chain[0], isRunning, unreadOf)?.id, "s2");
  assert.equal(latestFireStage(chain, chain[0], () => false, unreadOf)?.id, "s1");
  assert.equal(latestFireStage(chain, chain[0])?.id, "s2");
});

test("旧 [WF] 子阶段只有父链标记时，空闲未读导航仍可打开历史记录", () => {
  const root = { id: "root", title: "历史目标", createdAt: 0, cwd: "x" };
  const child = { id: "legacy", parentThreadId: root.id, title: "[WF] 旧阶段", createdAt: 1, cwd: "x" };
  assert.equal(latestFireStage([root, child], root, () => false, (id) => id === child.id ? 1 : 0, "unread")?.id, child.id);
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

test("链上多个阶段同时运行时取最新阶段，不被旧 stage 未读抢占", () => {
  assert.equal(latestFireStage(runningChain, runningChain[0], () => true, unreadOf)?.id, "s2");
});

test("链上没有 stage 节点时保持原行为：打开被点击的会话本身", () => {
  const plain = [{ id: "a", title: "普通会话", createdAt: 1, cwd: "x" }];
  assert.equal(latestFireStage(plain, plain[0], isRunning, unreadOf), undefined);
});

test("并行子 Agent 保留父链分组，但运行、未读和链尖都不抢自动导航", () => {
  const root = runningChain[0];
  const child = { id: "agent", parentThreadId: root.id, subagent: true, title: "[Agent] 检查接口", createdAt: 9, cwd: "other" };
  const threads = [root, child];
  assert.equal(chainGroupAnchor(threads, child), root);
  for (const prefer of ["running", "unread"]) {
    assert.equal(latestFireStage(threads, root, id => id === child.id, () => 3, prefer), undefined);
    assert.equal(latestFireStage(threads, child, () => true, () => 3, prefer), undefined);
  }
  assert.equal(nextRunningThread(threads, root.id, { agent: true }), undefined);
  assert.equal(nextRunningThread(threads, child.id, { root: true, agent: true }), root);
  // 即使旧数据误带 stage 标记，子 Agent 也不能成为接力目标。
  const mixed = [...runningChain, { ...child, stageSourceThreadId: root.id }];
  assert.equal(latestFireStage(mixed, root, id => id === child.id)?.id, "s2");
  assert.equal(latestFireStage(mixed, root, () => false, () => 0, "running")?.id, "s2");
  assert.equal(nextRunningThread(mixed, root.id, { s1: true, agent: true })?.id, "s1");
});
