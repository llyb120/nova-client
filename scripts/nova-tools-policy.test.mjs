import assert from "node:assert/strict";
import {
  codebuddyNovaTools,
  novaFastContextEnabled,
  novaToolsAttached,
  novaToolsMcpEnv,
  novaToolsPolicy,
  novaToolsScript,
  opencodeNovaTools,
  prefixFirstTextPart,
} from "./nova-tools-policy.mjs";

const SCRIPT = "/nova/runtime/nova-tools-mcp.mjs";
const exists = (path) => path === SCRIPT;

// --- env switches -----------------------------------------------------------
process.env.NOVA_FAST_CONTEXT = "1";
assert.equal(novaFastContextEnabled(), true);
process.env.NOVA_FAST_CONTEXT = "0";
assert.equal(novaFastContextEnabled(), false);
process.env.NOVA_FAST_CONTEXT = "1";

delete process.env.NOVA_TOOLS_MCP_SCRIPT;
assert.equal(novaToolsScript(exists), undefined);
process.env.NOVA_TOOLS_MCP_SCRIPT = SCRIPT;
assert.equal(novaToolsScript(exists), SCRIPT);
assert.equal(novaToolsScript(() => false), undefined);

// --- attachment decision ----------------------------------------------------
assert.equal(novaToolsAttached({ mode: "build", cwd: "/repo" }, { fileExists: exists }), true);
assert.equal(novaToolsAttached({ mode: "plan", cwd: "/repo" }, { fileExists: exists }), true, "plan keeps find_symbols/fast_context");
process.env.NOVA_FAST_CONTEXT = "0";
assert.equal(novaToolsAttached({ mode: "plan", cwd: "/repo" }, { fileExists: exists }), false, "plan without fast context exposes no tools");
assert.equal(novaToolsAttached({ mode: "build", cwd: "/repo" }, { fileExists: exists }), true, "build keeps edit_files");
process.env.NOVA_FAST_CONTEXT = "1";
assert.equal(novaToolsAttached({ mode: "build", cwd: "/repo" }, { fileExists: () => false }), false);

// --- MCP env ----------------------------------------------------------------
process.env.NOVA_CONTEXT_SERVICE_ENDPOINT = "http://127.0.0.1:9";
process.env.NOVA_CONTEXT_SERVICE_TOKEN = "token-1";
assert.deepEqual(novaToolsMcpEnv("/repo", { fastContext: true, readOnly: false }), {
  NOVA_TOOLS_CWD: "/repo",
  NOVA_FAST_CONTEXT: "1",
  NOVA_CONTEXT_SERVICE_ENDPOINT: "http://127.0.0.1:9",
  NOVA_CONTEXT_SERVICE_TOKEN: "token-1",
});
assert.deepEqual(novaToolsMcpEnv("/repo", { fastContext: false, readOnly: true }), {
  NOVA_TOOLS_CWD: "/repo",
  NOVA_FAST_CONTEXT: "0",
  NOVA_TOOLS_READ_ONLY: "1",
  NOVA_CONTEXT_SERVICE_ENDPOINT: "http://127.0.0.1:9",
  NOVA_CONTEXT_SERVICE_TOKEN: "token-1",
});

// --- CodeBuddy wiring -------------------------------------------------------
const codebuddy = codebuddyNovaTools({ mode: "build", cwd: "/repo" }, { fileExists: exists });
assert.deepEqual(codebuddy.mcpServers["nova-tools"], {
  type: "stdio",
  command: process.execPath,
  args: [SCRIPT],
  env: {
    NOVA_TOOLS_CWD: "/repo",
    NOVA_FAST_CONTEXT: "1",
    NOVA_CONTEXT_SERVICE_ENDPOINT: "http://127.0.0.1:9",
    NOVA_CONTEXT_SERVICE_TOKEN: "token-1",
  },
});
assert.equal(typeof codebuddy.systemPrompt.append, "string");
assert.match(codebuddy.systemPrompt.append, /fast_context/);
assert.match(codebuddy.systemPrompt.append, /edit_files/);
assert.match(codebuddy.systemPrompt.append, /hard constraints/);
assert.doesNotMatch(codebuddy.systemPrompt.append, /read-only/);

const codebuddyPlan = codebuddyNovaTools({ mode: "plan", cwd: "/repo" }, { fileExists: exists });
assert.equal(codebuddyPlan.mcpServers["nova-tools"].env.NOVA_TOOLS_READ_ONLY, "1");
assert.doesNotMatch(codebuddyPlan.systemPrompt.append, /edit_files/);
assert.match(codebuddyPlan.systemPrompt.append, /read-only/);

assert.deepEqual(codebuddyNovaTools({ mode: "build" }, { fileExists: () => false }), {}, "missing script skips MCP");

// --- opencode wiring --------------------------------------------------------
const opencode = opencodeNovaTools({ mode: "build", cwd: "/repo" }, { fileExists: exists });
assert.deepEqual(opencode.mcp["nova-tools"], {
  type: "local",
  command: [process.execPath, SCRIPT],
  environment: {
    NOVA_TOOLS_CWD: "/repo",
    NOVA_FAST_CONTEXT: "1",
    NOVA_CONTEXT_SERVICE_ENDPOINT: "http://127.0.0.1:9",
    NOVA_CONTEXT_SERVICE_TOKEN: "token-1",
  },
  enabled: true,
});
assert.deepEqual(opencodeNovaTools({ mode: "build" }, { fileExists: () => false }), {});

// --- policy content ---------------------------------------------------------
const noFast = novaToolsPolicy({ fastContext: false, readOnly: false });
assert.match(noFast, /edit_files/);
assert.doesNotMatch(noFast, /call fast_context once/);
const readOnly = novaToolsPolicy({ fastContext: true, readOnly: true });
assert.doesNotMatch(readOnly, /edit_files/);
assert.match(readOnly, /do not modify files/);

// --- first-text-part prefix -------------------------------------------------
const parts = [
  { type: "file", filename: "a.ts" },
  { type: "text", text: "修复构建" },
];
assert.deepEqual(prefixFirstTextPart(parts, "POLICY"), [
  { type: "file", filename: "a.ts" },
  { type: "text", text: "POLICY\n\n修复构建" },
]);
assert.equal(prefixFirstTextPart([{ type: "file" }], "POLICY")[0].type, "file", "no text part stays unchanged");
assert.deepEqual(parts[1], { type: "text", text: "修复构建" }, "input parts are not mutated");
