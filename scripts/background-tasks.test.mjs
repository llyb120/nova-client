import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import ts from "typescript";

// 不挂载 Solid/Tauri：直接跑 src/backgroundTasks.ts 的判定。
const js = ts.transpileModule(
  readFileSync(new URL("../src/backgroundTasks.ts", import.meta.url), "utf8"),
  { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None } },
).outputText;
const { backgroundTasks } = new Function("exports", `${js}\nreturn { backgroundTasks };`)({});

const tool = (id, title, text) => ({
  type: "tool", id, ts: 0, toolCallId: String(id), title, kind: "execute", status: "completed",
  content: [{ type: "content", content: { type: "text", text } }], locations: [],
});
const system = (id, level, text) => ({ type: "system", id, level, text, ts: 0 });
const turn = id => ({ type: "turn", id, ts: 0, durationMs: 1, stopReason: "end_turn" });
const bash = (id, job = 7) => tool(id, "cargo test --all", `命令超过 10s 仍未结束，已转到后台：job_id=${job}。现在先处理不依赖它的工作`);

test("bash 转后台算运行中，交付状态行把同一任务标成已结束", () => {
  assert.deepEqual(backgroundTasks([bash(1)]), [{ id: 7, command: "cargo test --all", running: true }]);
  assert.deepEqual(backgroundTasks([bash(1), system(2, "background", "后台任务 7 已结束")]), [
    { id: 7, command: "cargo test --all", running: false },
  ]);
});

test("bash_poll 取回结果也算结束", () => {
  const poll = tool(2, "Lyra / bash_poll", "[后台任务 7 已完成] exit=0\n\nrunning 16 tests");
  assert.deepEqual(backgroundTasks([bash(1), poll]), [{ id: 7, command: "cargo test --all", running: false }]);
});

test("运行中的排在前面，同一任务只出现一次", () => {
  const tasks = backgroundTasks([bash(1), tool(2, "npm run build", "已转到后台：job_id=9。"), system(3, "background", "后台任务 7 已结束")]);
  assert.deepEqual(tasks.map(task => [task.id, task.running]), [[9, true], [7, false]]);
});

test("只看最后一条 turn 之后：上一轮留下的任务不会挂在这一轮的卡片上", () => {
  assert.deepEqual(backgroundTasks([bash(1), turn(2)]), []);
  assert.deepEqual(backgroundTasks([bash(1), turn(2), bash(3, 11)]), [{ id: 11, command: "cargo test --all", running: true }]);
  // 本轮没起过的任务（上一轮留下的 poll 结果）不凭空出现；等待行也不占位置。
  const poll = tool(4, "Lyra / bash_poll", "[后台任务 11 已完成] exit=0");
  assert.deepEqual(backgroundTasks([poll, system(5, "task-wait", "等待后台任务或分身结果（未结束，不发起模型请求）")]), []);
  // 但交付行确实发生了：上一轮起的任务交回时留一条已结束，命令未知。
  assert.deepEqual(backgroundTasks([system(6, "background", "后台任务 12 已结束")]), [{ id: 12, command: "", running: false }]);
});

test("卡片挂在输入框上方，状态行由卡片接管", () => {
  const composer = readFileSync(new URL("../src/components/Composer.tsx", import.meta.url), "utf8");
  assert.match(composer, /const background = createMemo\(\(\) => \(running\(\) \? backgroundTasks\(state\.items\) : \[\]\)\);/);
  assert.match(composer, /<Show when=\{background\(\)\.length > 0\}>/);
  const card = composer.indexOf('class="background-tasks"');
  const input = composer.indexOf('class={["composer", { "is-dragging"');
  assert.ok(card > 0 && input > card, "卡片要在输入框之前");
});
