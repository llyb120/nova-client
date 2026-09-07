import assert from "node:assert/strict";
import { test } from "node:test";
import {
  canonicalStopReason,
  isManualStop,
  isNormalStop,
} from "../src/workflow/stopReason.ts";

test("内部 snake_case 原样保留，归一化是幂等的", () => {
  assert.equal(canonicalStopReason("end_turn"), "end_turn");
  assert.equal(canonicalStopReason("max_turn_requests"), "max_turn_requests");
  assert.equal(canonicalStopReason("cancelled"), "cancelled");
  assert.equal(canonicalStopReason("force_cancelled"), "force_cancelled");
});

// 回归：ACP 是 camelCase 协议（响应字段即 stopReason），部分实现把枚举名
// END_TURN / MAX_TURN_REQUESTS 原样透传。此前按全等比较会让一次正常收尾
// 被判成异常，走 suspendWorkflow 挂起，整条链永久留在室女座且无任何界面提示。
test("camelCase 枚举名归一化到内部口径", () => {
  assert.equal(canonicalStopReason("endTurn"), "end_turn");
  assert.equal(canonicalStopReason("maxTurnRequests"), "max_turn_requests");
  assert.equal(canonicalStopReason("forceCancelled"), "force_cancelled");
  assert.equal(canonicalStopReason("END_TURN"), "end_turn");
});

test("正常/中止判定按归一化后的口径分流", () => {
  assert.ok(isNormalStop("endTurn"));
  assert.ok(isNormalStop("end_turn"));
  assert.ok(isManualStop("forceCancelled"));
  // 连字符与大小写混排同样收敛。
  assert.ok(isNormalStop("End-Turn"));
});

test("真正的异常收尾仍要挂起等用户补充，不能被放宽成正常", () => {
  for (const reason of ["refusal", "error", "aborted", "max_tokens"]) {
    assert.ok(!isNormalStop(reason), reason);
    assert.ok(!isManualStop(reason), reason);
  }
});

test("空值不炸，且不算正常收尾", () => {
  for (const reason of [null, undefined, ""]) {
    assert.equal(canonicalStopReason(reason), "");
    assert.ok(!isNormalStop(reason));
    assert.ok(!isManualStop(reason));
  }
});
