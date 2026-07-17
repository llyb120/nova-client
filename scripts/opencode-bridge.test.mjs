import assert from "node:assert/strict";

process.env.NOVA_OPENCODE_BRIDGE_TEST = "1";
const { automaticPermissionReply } = await import("./opencode-bridge.mjs");

assert.equal(automaticPermissionReply("build"), "always");
assert.equal(automaticPermissionReply("plan"), undefined);
