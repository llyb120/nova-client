// Run: node --test scripts/prompt-display.test.mjs
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { transformSync } from "esbuild";

const source = readFileSync(new URL("../src/promptDisplay.ts", import.meta.url), "utf8");
const js = transformSync(source, { loader: "ts", format: "cjs", target: "esnext" }).code;

function loadModule(store = new Map()) {
  const localStorage = { getItem: (k) => store.get(k) ?? null, setItem: (k, v) => store.set(k, v) };
  const module = { exports: {} };
  new Function("module", "exports", "localStorage", js)(module, module.exports, localStorage);
  return module.exports;
}

test("系统改写的提示词映射回用户输入，并跨重载保留", () => {
  const store = new Map();
  const a = loadModule(store);
  a.rememberPromptDisplay("请以快速小改动方式完成…\n\n任务：\n修复类型错误", "/easy 修复类型错误");
  a.rememberPromptDisplay("原样发送", "原样发送");
  assert.equal(a.promptDisplayOf("请以快速小改动方式完成…\n\n任务：\n修复类型错误\n"), "/easy 修复类型错误");
  assert.equal(a.promptDisplayOf("原样发送"), undefined);
  assert.equal(a.promptDisplayOf("别的内容"), undefined);
  assert.equal(loadModule(store).promptDisplayOf("请以快速小改动方式完成…\n\n任务：\n修复类型错误"), "/easy 修复类型错误");
});

test("超过上限时淘汰最早的记录", () => {
  const m = loadModule();
  for (let i = 0; i < 301; i++) m.rememberPromptDisplay(`expanded ${i}`, `typed ${i}`);
  assert.equal(m.promptDisplayOf("expanded 0"), undefined);
  assert.equal(m.promptDisplayOf("expanded 300"), "typed 300");
});
