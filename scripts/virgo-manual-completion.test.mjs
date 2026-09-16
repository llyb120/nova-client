import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { runInNewContext } from "node:vm";
import ts from "typescript";
import { virgoChains } from "../src/virgoChains.ts";

const source = ts.createSourceFile("store.ts", readFileSync(new URL("../src/store.ts", import.meta.url), "utf8"), ts.ScriptTarget.Latest, true);
const fn = source.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === "virgoManualHidden");
assert.ok(fn);
const code = ts.transpile(fn.getText(source));
const threads = [{ id: "root" }, { id: "child", parentThreadId: "root" }];
const roots = new Set(["root"]);
let input = { threads, isRunning: () => false, advancingRoots: [], unfinishedRoots: [], queuedThreads: [] };
let saves = 0;
const context = {
  state: { threads },
  virgoManualRoots: roots,
  virgoManualRootsSnapshot: () => [...roots],
  zenRunningChains: () => virgoChains(input),
  setVirgoManualVersion() {},
  persistVirgoManualRoots: () => saves++,
  virgoChainIds: (ids) => new Set(ids.includes("root") ? threads.map((t) => t.id) : []),
};
const hidden = () => [...runInNewContext(`${code}\nvirgoManualHidden()`, context)];
// 初始化尚未取得会话快照时不能丢失持久化收纳。
context.state.threads = [];
assert.deepEqual(hidden(), []);
assert.ok(roots.has("root"));
context.state.threads = threads;
for (const active of [
  { isRunning: (id) => id === "child" },
  { advancingRoots: ["root"] },
  { unfinishedRoots: ["root"] },
  { queuedThreads: ["child"] },
]) {
  const idle = input;
  input = { ...idle, ...active };
  assert.deepEqual(hidden(), ["root", "child"]);
  assert.equal(saves, 0);
  input = idle;
}
assert.deepEqual(hidden(), []);
assert.equal(roots.size, 0);
assert.equal(saves, 1);
// 新回合不能被上一次快捷键遗留的标记再次收起。
input.isRunning = () => true;
assert.deepEqual(hidden(), []);
assert.equal(saves, 1);
console.log("virgo manual completion checks passed");
