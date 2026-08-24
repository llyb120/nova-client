import assert from "node:assert/strict";
import test from "node:test";

process.env.NOVA_CODEBUDDY_BRIDGE_TEST = "1";

const { createCodeBuddyUsageTracker } = await import("./codebuddy-bridge.mjs");

const usage = (input, output) => ({ input_tokens: input, output_tokens: output });

test("usage tracker accumulates distinct assistant messages", () => {
  const tracker = createCodeBuddyUsageTracker();
  assert.deepEqual(tracker.add("a", usage(10, 1)), { input_tokens: 10, output_tokens: 1 });
  assert.deepEqual(tracker.add("b", usage(20, 2)), { input_tokens: 30, output_tokens: 3 });
  assert.deepEqual(tracker.total, { input_tokens: 30, output_tokens: 3 });
});

test("usage tracker deduplicates replayed assistant messages by uuid", () => {
  const tracker = createCodeBuddyUsageTracker();
  // resume 会话重放历史消息：每个 uuid 只计一次。
  tracker.add("old-1", usage(100, 5));
  tracker.add("old-2", usage(200, 10));
  assert.equal(tracker.add("old-1", usage(100, 5)), undefined);
  assert.equal(tracker.add("old-2", usage(200, 10)), undefined);
  assert.deepEqual(tracker.add("new", usage(30, 3)), { input_tokens: 330, output_tokens: 18 });
  assert.deepEqual(tracker.total, { input_tokens: 330, output_tokens: 18 });
});

test("usage tracker tolerates missing uuid or usage", () => {
  const tracker = createCodeBuddyUsageTracker();
  assert.equal(tracker.add(undefined, undefined), undefined);
  assert.deepEqual(tracker.add(undefined, usage(5, 1)), { input_tokens: 5, output_tokens: 1 });
});
