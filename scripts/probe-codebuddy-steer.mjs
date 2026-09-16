// Live ACP check: node scripts/probe-codebuddy-steer.mjs <CodeBuddy bin script>
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";

assert(process.argv[2], "Pass the installed CodeBuddy bin/codebuddy path");
const cwd = mkdtempSync(join(tmpdir(), "nova-codebuddy-steer-"));
const child = spawn(process.execPath, [process.argv[2], "--acp", "--acp-transport", "stdio"], {
  cwd, windowsHide: true, stdio: ["pipe", "pipe", "pipe"],
});
child.stderr.resume();
const pending = new Map();
let nextId = 0;
let sessionId;
let mainDone = false;
let steer;
let output = "";
const marker = `STEER_${Date.now()}`;
const send = (message) => child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", ...message })}\n`);
function request(method, params) {
  return new Promise((resolve, reject) => {
    const id = ++nextId;
    pending.set(id, { resolve, reject });
    send({ id, method, params });
  });
}
function fail(error) {
  for (const waiter of pending.values()) waiter.reject(error);
  pending.clear();
}
child.on("error", fail);
child.on("exit", (code) => fail(new Error(`CodeBuddy exited: ${code}`)));
const lines = createInterface({ input: child.stdout });
lines.on("line", (line) => {
  let message;
  try { message = JSON.parse(line); } catch { return; }
  if (message.method === "session/update") {
    const update = message.params.update;
    if (update.sessionUpdate === "agent_message_chunk") output += update.content?.text ?? "";
    if (sessionId && !steer && ["agent_message_chunk", "agent_thought_chunk"].includes(update.sessionUpdate)) {
      assert(!mainDone, "Main prompt ended before injection");
      console.log("Injecting while main prompt is still running");
      steer = request("session/steer", { sessionId, contentBlocks: [{ type: "text",
        text: `Change of task: stop the list now. Reply with exactly ${marker}. Do not use tools.`,
      }] }).then((result) => { console.log("Injection RPC acknowledged"); return { result }; }, (error) => ({ error }));
    }
  } else if (message.method && message.id !== undefined) {
    if (message.method === "session/request_permission") {
      send({ id: message.id, result: { outcome: { outcome: "cancelled" } } });
    } else {
      send({ id: message.id, error: { code: -32601, message: "Unsupported in probe" } });
    }
  } else if (pending.has(message.id)) {
    const waiter = pending.get(message.id);
    pending.delete(message.id);
    if (message.error) waiter.reject(new Error(JSON.stringify(message.error)));
    else waiter.resolve(message.result);
  }
});
const timeout = setTimeout(() => {
  fail(new Error("ACP probe timed out after 120 seconds"));
  child.kill();
}, 120_000);
try {
  const init = await request("initialize", { protocolVersion: 1,
    clientInfo: { name: "nova-steer-probe", version: "1" }, clientCapabilities: {},
  });
  console.log("Agent:", JSON.stringify(init.agentInfo));
  const session = await request("session/new", { cwd, mcpServers: [] });
  sessionId = session.sessionId;
  assert(sessionId);
  const result = await request("session/prompt", { sessionId, prompt: [{ type: "text",
    text: "Do not use tools or access files. Write a numbered list of 300 distinct short facts about integers, one fact per line.",
  }] });
  mainDone = true;
  console.log("Main result:", JSON.stringify(result));
  assert(steer, "No streaming event received before completion");
  const injected = await steer;
  if (injected.error) throw injected.error;
  console.log("Steer result:", JSON.stringify(injected.result));
  assert.equal(injected.result.steered, true);
  console.log("Output tail:", output.slice(-800));
  assert(output.includes(marker), "Agent did not acknowledge the injected marker");
  console.log("PASS: session/steer accepted and instruction followed before main prompt completed");
} finally {
  clearTimeout(timeout);
  lines.close();
  child.kill();
}
