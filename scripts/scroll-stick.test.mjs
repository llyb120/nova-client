import assert from "node:assert/strict";
import { test } from "node:test";
import { resolveScrollAfterLayout } from "../src/scrollStick.ts";

test("layout 期间用户滚离底部后不被拽回（切会话冷布局窗口的钉死回归）", () => {
  // await 前在底部（scrollY=maxScroll=1000）；布局让帧期间用户滚轮到 700，
  // 内容增长到 totalHeight=2050（viewH=1000 → 新 maxScroll=1050）。
  const r = resolveScrollAfterLayout({
    keepBottom: false, scrollY: 700, maxScrollBefore: 1000, totalHeight: 2050, viewH: 1000,
  });
  assert.equal(r.maxScroll, 1050);
  assert.equal(r.scrollY, 700);
});

test("布局期间无操作时保持吸底", () => {
  const r = resolveScrollAfterLayout({
    keepBottom: true, scrollY: 1000, maxScrollBefore: 1000, totalHeight: 2050, viewH: 1000,
  });
  assert.equal(r.scrollY, 1050);
  // keepBottom 为 false 但实时位置仍贴旧底部（如切会话重置后 scrollY=maxScroll=0）
  const reset = resolveScrollAfterLayout({
    keepBottom: false, scrollY: 0, maxScrollBefore: 0, totalHeight: 2050, viewH: 1000,
  });
  assert.equal(reset.scrollY, 1050);
});

test("内容收缩后滚动位置收敛到新 maxScroll", () => {
  const r = resolveScrollAfterLayout({
    keepBottom: false, scrollY: 900, maxScrollBefore: 1000, totalHeight: 800, viewH: 1000,
  });
  assert.equal(r.maxScroll, 0);
  assert.equal(r.scrollY, 0);
});
