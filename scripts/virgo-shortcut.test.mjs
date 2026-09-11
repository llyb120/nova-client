import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { runInNewContext } from "node:vm";
import ts from "typescript";

// 执行实际 store 函数，隔离 Tauri 和 UI 初始化。
const source = ts.createSourceFile("store.ts", readFileSync(new URL("../src/store.ts", import.meta.url), "utf8"), ts.ScriptTarget.Latest, true);
const functions = source.statements.filter((node) => ts.isFunctionDeclaration(node) &&
  ["hideCurrentThreadToVirgo", "virgoChainRoot"].includes(node.name?.text));
assert.equal(functions.length, 2);
const code = ts.transpile(functions.map((node) => node.getText(source).replace(/^export /, "")).join("\n"));

function check({ currentId = null, running = {}, hidden = [], zen = false, threads = [{ id: "first" }, { id: "current" }] }, expected, closes = false) {
  const roots = new Set(hidden);
  const effects = [];
  const context = {
    state: { currentId, running, threads },
    virgoManualRoots: roots,
    zenModeOn: () => zen,
    isPendingThreadId: (id) => id.startsWith("pending:"),
    setVirgoManualVersion: () => effects.push("version"),
    persistVirgoManualRoots: () => effects.push("persist"),
    closeThread: () => effects.push("close"),
    setView: (view) => effects.push(view),
    showToast: () => effects.push("toast"),
  };
  const result = runInNewContext(`${code}\nhideCurrentThreadToVirgo()`, context);
  assert.equal(result, expected !== null);
  assert.deepEqual([...roots], expected === null ? hidden : [...hidden, expected]);
  assert.deepEqual(effects, expected === null ? [] : ["version", "persist", ...(closes ? ["close", "home"] : []), "toast"]);
}

check({ currentId: "current", running: { first: true, current: true } }, "current", true);
check({ currentId: "current", running: { first: true } }, "first");
check({ running: { first: true, current: true } }, "first");
check({ currentId: "missing", running: { first: true } }, "first");
check({ currentId: "current" }, null);
check({}, null);
check({ running: { first: true, current: true }, hidden: ["first"] }, "current");
check({ running: { first: true }, hidden: ["first"] }, null);
check({ currentId: "pending:new", running: { "pending:new": true, first: true } }, "first");
check({ currentId: "current", running: { current: true }, zen: true }, null);
const threads = [{ id: "root" }, { id: "child", parentThreadId: "root" }, { id: "first" }];
check({ threads, currentId: "root", running: { child: true } }, "root", true);
check({ threads, running: { child: true, first: true }, hidden: ["root"] }, "first");
console.log("Virgo shortcut: 12 checks passed");
