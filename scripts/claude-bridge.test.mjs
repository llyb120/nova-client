import assert from "node:assert/strict";

process.env.NOVA_CLAUDE_BRIDGE_TEST = "1";
const { claudeModelOptions, claudeModelSelection } = await import("./claude-bridge.mjs");

assert.deepEqual(claudeModelOptions([
  {
    value: "sonnet",
    displayName: "Sonnet",
    description: "Balanced",
    supportsEffort: true,
    supportedEffortLevels: ["low", "high", "max"],
  },
  { value: "haiku", displayName: "Haiku", description: "Fast" },
]), [
  { value: "sonnet:low", name: "Sonnet Low", description: "Balanced" },
  { value: "sonnet:high", name: "Sonnet High", description: "Balanced" },
  { value: "sonnet:max", name: "Sonnet Max", description: "Balanced" },
  { value: "haiku", name: "Haiku", description: "Fast" },
]);
assert.deepEqual(claudeModelSelection("sonnet:xhigh"), { model: "sonnet", effort: "xhigh" });
assert.deepEqual(claudeModelSelection("opus[1m]"), { model: "opus[1m]" });
