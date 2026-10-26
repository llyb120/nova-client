import assert from "node:assert/strict";
import { test } from "node:test";
import { resolveUserScrollStick } from "../src/scrollStick.ts";

test("用户上滚即解除吸底，含慢扫 1-2px（流式钉回吞滚动回归）", () => {
  // 贴底(maxScroll=1000)时触控板慢扫上滚 1px：旧纯阈值会误判仍吸底
  assert.equal(resolveUserScrollStick(1000, 999, 1000), false);
  assert.equal(resolveUserScrollStick(1000, 900, 1000), false);
  // 下滚贴底 2px 内恢复吸底；未贴底不恢复
  assert.equal(resolveUserScrollStick(900, 999, 1000), true);
  assert.equal(resolveUserScrollStick(900, 950, 1000), false);
  // 贴底后继续下滚被钳位（位置未变）仍保持吸底
  assert.equal(resolveUserScrollStick(1000, 1000, 1000), true);
});
