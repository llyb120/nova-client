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

// 有未读时优先进入未读 stage（即便另一个 stage 正在运行）
{
  const threads = [
    root,
    meta("a", "[Fire] 阶段 1", 2, "root"),
    meta("b", "[Fire] 阶段 2", 3, "root"),
  ];
  const running = new Set(["b"]);
  const unread = { a: 1 };
  assert.equal(
    latestFireStage(threads, root, (id) => running.has(id), (id) => unread[id] ?? 0)?.id,
    "a",
  );
}

// 多个未读 → 取最早创建的一个
{
  const threads = [
    root,
    meta("a", "[Fire] 阶段 1", 2, "root"),
    meta("b", "[Fire] 阶段 2", 3, "a"),
  ];
  const unread = { a: 2, b: 1 };
  assert.equal(
    latestFireStage(threads, root, () => false, (id) => unread[id] ?? 0)?.id,
    "a",
  );
}

// 未读挂在非 Fire/WF 的中间会话上时不算，但会继续向下遍历
{
  const threads = [
    root,
    meta("s", "[Stage] 普通", 2, "root"),
    meta("c", "[WF] 节点", 3, "s"),
  ];
  const unread = { s: 1 };
  assert.equal(
    latestFireStage(threads, root, () => false, (id) => unread[id] ?? 0)?.id,
    "c",
  );
}

// 普通 /stage 链：root 是常规会话标题，stage 以 stageSourceThreadId 识别
// （标题可能已被自动改名），点击 root 直达运行中的 stage
{
  const stageRoot = meta("r2", "修 bug", 1);
  const threads = [
    stageRoot,
    { ...meta("s1", "已改名的 stage", 2, "r2"), stageSourceThreadId: "r2" },
  ];
  const running = new Set(["s1"]);
  assert.equal(
    latestFireStage(threads, stageRoot, (id) => running.has(id))?.id,
    "s1",
  );
}

// /stage 链都未运行 → 回退到最新创建的 stage（含嵌套 /stage）
{
  const stageRoot = meta("r3", "普通会话", 1);
  const threads = [
    stageRoot,
    { ...meta("s1", "[Stage] 第一次", 2, "r3"), stageSourceThreadId: "r3" },
    { ...meta("s2", "[Stage] 第二次", 3, "s1"), stageSourceThreadId: "s1" },
  ];
  assert.equal(latestFireStage(threads, stageRoot, () => false)?.id, "s2");
}

// 预检→开发子会话（无 stage 标记、root 非 Fire/WF）保持原行为：不自动跳转
{
  const preRoot = meta("p2", "预检会话", 1);
  const threads = [preRoot, meta("dev", "开发子会话", 2, "p2")];
  assert.equal(latestFireStage(threads, preRoot, () => true), undefined);
}

// 无 fire 子会话 → undefined；root 非 fire → undefined
assert.equal(latestFireStage([root], root), undefined);
assert.equal(
  latestFireStage([meta("p", "普通会话", 1)], meta("p", "普通会话", 1)),
  undefined,
);

console.log("thread-display tests passed");
