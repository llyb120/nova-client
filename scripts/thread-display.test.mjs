import assert from "node:assert/strict";
import { test } from "node:test";
import { chainGroupAnchor } from "../src/threadDisplay.ts";

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
