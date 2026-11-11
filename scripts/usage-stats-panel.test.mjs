import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const panel = readFileSync(new URL("../src/components/UsageStatsPanel.tsx", import.meta.url), "utf8");
const lib = readFileSync(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");

// 后端并发处理 invoke：先发的请求可能后返回，后返回的旧范围不能盖掉当前页签。
test("range switches keep only the latest response", () => {
  assert.match(panel, /const ticket = \+\+request;/);
  assert.equal(panel.match(/if \(ticket === request\)/g)?.length, 3);
  assert.match(panel, /loaded\(\)\?\.range === range\(\)/, "显示的必须是当前范围的数据");
});

// 请求期间禁掉页签会让每次切换都要等上一次跑完，切完还得手动再点刷新。
test("range tabs stay clickable while a request is in flight", () => {
  assert.equal(panel.match(/disabled=\{busy\(\)\}/g)?.length, 1, "只有刷新按钮按忙碌禁用");
});

// 读累计值要等会话存储锁并遍历全部轮次，同步命令会卡住界面线程。
test("usage stats runs off the UI thread", () => {
  const decl = lib.indexOf("async fn usage_stats(");
  assert.ok(decl > 0, "usage_stats 必须是 async 命令（同步命令会在界面线程等存储锁）");
  const before = lib.slice(Math.max(0, decl - 400), decl);
  assert.ok(/#\[tauri::command\]\r?\n(?:\s*\/\/\/[^\n]*\r?\n)*\s*$/.test(before), "紧挨命令宏，没有被改成别的属性");
});
