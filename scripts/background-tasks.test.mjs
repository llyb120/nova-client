import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { transformSync } from "esbuild";
import { createEffect, createMemo, createRoot, createSignal, createStore, flush } from "@solidjs/signals";
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
const bash = (id, job = 7) => ({ ...tool(id, "cargo test --all", `命令超过 10s 仍未结束，已转到后台：job_id=${job}。现在先处理不依赖它的工作`), ts: 1000, rawOutput: { jobId: job } });

test("bash 转后台算运行中，交付状态行把同一任务标成已结束", () => {
  assert.deepEqual(backgroundTasks([bash(1)]), [{ id: 7, command: "cargo test --all", running: true, startedAt: 1000 }]);
  assert.deepEqual(backgroundTasks([bash(1), system(2, "background", "后台任务 7 已结束")]), [
    { id: 7, command: "cargo test --all", running: false, startedAt: 1000 },
  ]);
});

test("bash_poll 取回结果也算结束", () => {
  const poll = { ...tool(2, "Lyra / bash_poll", "[后台任务 7 已完成] exit=0\n\nrunning 16 tests"), kind: "other" };
  assert.deepEqual(backgroundTasks([bash(1), poll]), [{ id: 7, command: "cargo test --all", running: false, startedAt: 1000 }]);
  const running = { ...tool(3, "Lyra / bash_poll", "后台任务 7 仍在运行。"), kind: "other", rawInput: { job_id: 7 } };
  const finished = { ...tool(4, "Lyra / bash_poll", "命令输出"), kind: "other", rawInput: { job_id: 7 } };
  assert.equal(backgroundTasks([bash(1), running])[0].running, true);
  assert.equal(backgroundTasks([bash(1), finished])[0].running, false);
});

test("运行中的排在前面，同一任务只出现一次", () => {
  const tasks = backgroundTasks([bash(1), { ...bash(2, 9), title: "npm run build" }, system(3, "background", "后台任务 7 已结束")]);
  assert.deepEqual(tasks.map(task => [task.id, task.running]), [[9, true], [7, false]]);
});

test("只看最后一条 turn 之后：上一轮留下的任务不会挂在这一轮的卡片上", () => {
  assert.deepEqual(backgroundTasks([bash(1), turn(2)]), []);
  assert.deepEqual(backgroundTasks([bash(1), turn(2), bash(3, 11)]), [{ id: 11, command: "cargo test --all", running: true, startedAt: 1000 }]);
  // 本轮没起过的任务（上一轮留下的 poll 结果）不凭空出现；等待行也不占位置。
  const poll = tool(4, "Lyra / bash_poll", "[后台任务 11 已完成] exit=0");
  assert.deepEqual(backgroundTasks([poll, system(5, "task-wait", "等待后台任务或分身结果（未结束，不发起模型请求）")]), []);
  // 但交付行确实发生了：上一轮起的任务交回时留一条已结束，命令未知。
  assert.deepEqual(backgroundTasks([system(6, "background", "后台任务 12 已结束")]), [{ id: 12, command: "", running: false, startedAt: null }]);
});

test("普通命令的输出不能伪造后台任务或结束状态", () => {
  const rg = tool(2, "rg job_id scripts", 'scripts/background-tasks.test.mjs:34: "已转到后台：job_id=9。"');
  assert.deepEqual(backgroundTasks([rg]), []);
  const spoof = { ...tool(3, "echo", "命令超过 10s 仍未结束，已转到后台：job_id=9。"), rawOutput: { exitCode: 0 } };
  assert.deepEqual(backgroundTasks([spoof]), [], "有正常退出元数据时不认伪造的首行");
  assert.equal(backgroundTasks([bash(1), tool(4, "rg", "后台任务 7 已结束")])[0].running, true);
  const legacy = { ...bash(5, 11), rawOutput: { durationMs: 10000 } };
  assert.equal(backgroundTasks([legacy])[0].id, 11, "旧会话仍按完整转后台提示识别");
});

test("面板默认折叠已结束任务，停止只调用当前会话的目标任务", async () => {
  const source = readFileSync(new URL("../src/components/Composer.tsx", import.meta.url), "utf8");
  const file = ts.createSourceFile("Composer.tsx", source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const component = file.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === "Composer");
  const names = new Set(["background", "backgroundRunning", "backgroundStopping", "backgroundHidden", "showFinishedTasks", "stoppingTasks", "visibleBackground", "stopBackgroundTask"]);
  const declarations = component.body.statements.filter(node => ts.isVariableStatement(node)
    && node.declarationList.declarations.some(item => ts.isIdentifier(item.name) ? names.has(item.name.text)
      : ts.isArrayBindingPattern(item.name) && item.name.elements.some(part => ts.isBindingElement(part) && names.has(part.name.getText(file)))));
  const reset = component.body.statements.find(node => ts.isExpressionStatement(node) && node.getText(file).includes("setShowFinishedTasks(false)"));
  const code = transformSync(`${declarations.map(node => node.getText(file)).join("\n")}\n${reset.getText(file)}`, { loader: "ts", target: "esnext" }).code;
  const mount = new Function("createMemo", "createSignal", "createEffect", "state", "running", "backgroundTasks", "api", "showToast", `${code}; return { background, visibleBackground, showFinishedTasks, setShowFinishedTasks, stoppingTasks, stopBackgroundTask };`);
  const [state, setState] = createStore({ currentId: "thread-a", items: [bash(1), bash(2, 9), system(3, "background", "后台任务 9 已结束")] });
  let reject, resolve, calls = [], messages = [], dispose;
  const api = { cancelBackgroundTask: (threadId, id) => { calls.push([threadId, id]); return new Promise((ok, fail) => { resolve = ok; reject = fail; }); } };
  const view = createRoot(cleanup => { dispose = cleanup; return mount(createMemo, createSignal, createEffect, state, () => true, backgroundTasks, api, value => messages.push(value)); });
  try {
    flush();
    assert.deepEqual(view.visibleBackground().map(task => task.id), [7]);
    view.setShowFinishedTasks(true); flush();
    assert.deepEqual(view.visibleBackground().map(task => task.id), [7, 9]);
    view.setShowFinishedTasks(false); flush();
    const pending = view.stopBackgroundTask(7); flush();
    assert.deepEqual(view.visibleBackground(), [], "请求停止后收起该行");
    assert.deepEqual(calls, [["thread-a", 7]]);
    assert.equal(view.stoppingTasks().has(7), true);
    reject(new Error("不可停止"));
    await pending; flush();
    assert.equal(view.stoppingTasks().has(7), false);
    assert.match(messages[0], /不可停止/);
    assert.deepEqual(view.visibleBackground().map(task => task.id), [7], "停止失败要恢复任务行");
    const stopped = view.stopBackgroundTask(7); flush();
    assert.deepEqual(view.visibleBackground(), [], "请求停止后先收起，失败时恢复");
    resolve(); await stopped; flush();
    assert.equal(view.stoppingTasks().has(7), true, "停止请求成功后等待实际结束状态");
    setState(s => { s.items.push(system(4, "background", "后台任务 7 已结束")); }); flush();
    assert.deepEqual(view.visibleBackground(), [], "确认结束后默认折叠");
    view.setShowFinishedTasks(true); flush();
    assert.deepEqual(view.visibleBackground().map(task => task.id), [7, 9]);
    setState(s => { s.currentId = "thread-b"; }); flush();
    assert.equal(view.showFinishedTasks(), false, "切换会话不继承展开和停止状态");
  } finally { dispose(); }
});

test("卡片挂在输入框上方，状态行由卡片接管", () => {
  const composer = readFileSync(new URL("../src/components/Composer.tsx", import.meta.url), "utf8");
  assert.match(composer, /const background = createMemo\(\(\) => \(running\(\) \? backgroundTasks\(state\.items\) : \[\]\)\);/);
  assert.match(composer, /<Show when=\{background\(\)\.length > 0\}>/);
  const card = composer.indexOf('class="background-tasks"');
  const input = composer.indexOf('class={["composer", { "is-dragging"');
  assert.ok(card > 0 && input > card, "卡片要在输入框之前");
  assert.match(composer, /aria-expanded=\{showFinishedTasks\(\) \? "true" : "false"\}/);
  assert.match(composer, /onClick=\{\(\) => void stopBackgroundTask\(task\.id\)\}/);
  assert.match(composer, /runClock\(\) - task\.startedAt/);
});
