// latestFireStage 选择逻辑自检：node scripts/thread-display.test.mjs
import assert from "node:assert/strict";
import { latestFireStage } from "../src/threadDisplay.ts";

const meta = (id, title, createdAt, parentThreadId = null) => ({
  id,
  title,
  createdAt,
  parentThreadId,
});

const root = meta("root", "[Fire] 目标", 1);

// 多 stage 并行：最新创建的未运行，较早创建的仍在运行 → 选中运行中的
{
  const threads = [
    root,
    meta("a", "[Fire] 阶段 1", 2, "root"),
    meta("b", "[Fire] 阶段 2", 3, "root"),
  ];
  const running = new Set(["a"]);
  assert.equal(
    latestFireStage(threads, root, (id) => running.has(id))?.id,
    "a",
  );
}

// 多个同时运行 → 取最新创建的运行中 stage
{
  const threads = [
    root,
    meta("a", "[Fire] 阶段 1", 2, "root"),
    meta("b", "[Fire] 阶段 2", 3, "root"),
  ];
  const running = new Set(["a", "b"]);
  assert.equal(
    latestFireStage(threads, root, (id) => running.has(id))?.id,
    "b",
  );
}

// 都不运行 → 回退到最新创建的（原行为）
{
  const threads = [
    root,
    meta("a", "[Fire] 阶段 1", 2, "root"),
    meta("b", "[WF] 节点", 3, "a"),
  ];
  assert.equal(
    latestFireStage(threads, root, () => false)?.id,
    "b",
  );
}

// 非 Fire/WF 标题的子会话不作为候选，但链继续向下遍历
{
  const threads = [
    root,
    meta("s", "[Stage] 普通", 2, "root"),
    meta("c", "[WF] 节点", 3, "s"),
  ];
  assert.equal(
    latestFireStage(threads, root, () => false)?.id,
    "c",
  );
}

// 无 fire 子会话 → undefined；root 非 fire → undefined
assert.equal(latestFireStage([root], root), undefined);
assert.equal(
  latestFireStage([meta("p", "普通会话", 1)], meta("p", "普通会话", 1)),
  undefined,
);

console.log("thread-display tests passed");
