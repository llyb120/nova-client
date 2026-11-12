import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import ts from "typescript";

// 不挂载 Solid/Tauri：直接跑 src/turnResume.ts 的判定。
const js = ts.transpileModule(
  readFileSync(new URL("../src/turnResume.ts", import.meta.url), "utf8"),
  { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None } },
).outputText;
const { interruptedTurn } = new Function(
  "exports",
  `${js}\nreturn { interruptedTurn };`,
)({});

const user = (id) => ({ type: "user", id, text: "做点事", ts: 0 });
const assistant = (id) => ({ type: "assistant", id, text: "好了", ts: 0 });
const tool = (id) => ({ type: "tool", id, ts: 0 });
const system = (id) => ({ type: "system", id, text: "提示", level: "info", ts: 0 });
const turn = (id, stopReason) => ({ type: "turn", id, ts: 0, durationMs: 1, stopReason });

test("正常收尾的一轮不算中断，也不展示继续", () => {
  assert.equal(interruptedTurn([]), false);
  assert.equal(interruptedTurn([user(1), assistant(2), turn(3, "end_turn")]), false);
  assert.equal(interruptedTurn([user(1), turn(2, "max_turn_requests")]), false);
  // 收尾后追加的系统提示不属于中断。
  assert.equal(interruptedTurn([user(1), assistant(2), turn(3, "end_turn"), system(4)]), false);
});

test("中断处的继续按钮接在会话视图里，点击就是发一条「继续」", () => {
  const view = readFileSync(new URL("../src/components/ChatView.tsx", import.meta.url), "utf8");
  assert.match(view, /<Show when=\{interrupted\(\)\}>/);
  assert.match(view, /sendPrompt\("继续"\)/);
  // 运行中/预览历史/未加载完时不显示：这些条件就在 memo 里。
  assert.match(view, /!isRunning\(\) &&\s+interruptedTurn\(displayedItems\(\)\)/);
});

test("停止、出错、进展中与进程被杀都停在中断处", () => {
  assert.equal(interruptedTurn([user(1), assistant(2), turn(3, "cancelled")]), true);
  assert.equal(interruptedTurn([user(1), turn(2, "error")]), true);
  // 进程被杀：来不及写轮次标记。
  assert.equal(interruptedTurn([user(1), assistant(2), tool(3)]), true);
  assert.equal(interruptedTurn([user(1)]), true);
  // 只认最后一条用户消息之后的状态：旧轮正常收尾不影响新一轮。
  assert.equal(interruptedTurn([user(1), turn(2, "end_turn"), user(3), tool(4)]), true);
  // 中断后重发并正常收尾：继续按钮收起。
  assert.equal(interruptedTurn([user(1), turn(2, "cancelled"), user(3), turn(4, "end_turn")]), false);
});
