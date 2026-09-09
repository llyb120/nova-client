import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { createServer } from "node:net";
import { createServer as createHttpServer } from "node:http";
import { spawn } from "node:child_process";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { AppServer, forkThread, inputParts, normalizeItem, runTurn, threadOptions } from "./codex-app-server.mjs";

const request = { action: "prompt", cwd: process.cwd(), model: "test-model", reasoningEffort: "high", parts: [{ type: "text", text: "hello" }] };
const contextEnv = { NOVA_CONTEXT_SERVICE_ENDPOINT: "test-endpoint", NOVA_CONTEXT_SERVICE_TOKEN: "test-token", NOVA_PONYTAIL: "1" };

class FakeServer {
  constructor(onStart) {
    this.calls = [];
    this.onStart = onStart;
    this.failure = new Promise((_, reject) => { this.reject = reject; });
    this.failure.catch(() => {});
  }
  fail(error) { this.reject(error); }
  async initialize() { this.calls.push(["initialize"]); }
  async close() { this.closed = true; }
  notify(method, params) { this.onNotification({ method, params: { threadId: "thread-1", ...params } }); }
  respond(id, result) { this.calls.push(["response", { id, result }]); }
  async rpc(method, params) {
    this.calls.push([method, params]);
    if (method === "thread/start" || method === "thread/resume") return { thread: { id: "thread-1" } };
    if (method === "thread/read") return { thread: { turns: [{ id: "turn-1" }, { id: "turn-2" }] } };
    if (method === "thread/fork") return { thread: { id: "fork-1" } };
    if (method === "turn/start") {
      this.notify("turn/started", { turn: { id: "turn-1" } });
      this.onStart?.(this);
      return { turn: { id: "turn-1" } };
    }
    if (method === "turn/interrupt") this.finish("interrupted");
    if (method === "turn/steer") this.finish();
    return {};
  }
  finish(status = "completed", error) { this.notify("turn/completed", { turn: { id: "turn-1", status, error } }); }
}

async function withoutPolaris(fn) {
  const previous = process.env.NOVA_FAST_CONTEXT;
  process.env.NOVA_FAST_CONTEXT = "0";
  try { return await fn(); }
  finally {
    if (previous === undefined) delete process.env.NOVA_FAST_CONTEXT;
    else process.env.NOVA_FAST_CONTEXT = previous;
  }
}

test("thread configuration mounts Polaris and injects guidance without changing user input", () => {
  const options = threadOptions({ ...request, mode: "plan" }, contextEnv);
  const mcp = options.config["mcp_servers.nova-tools"];
  assert.equal(mcp.command, process.execPath);
  assert.equal(mcp.required, true);
  assert.equal(mcp.env.NOVA_TOOLS_CWD, request.cwd);
  assert.equal(mcp.env.NOVA_CONTEXT_SERVICE_TOKEN, "test-token");
  assert.equal(mcp.env.NOVA_MCP_DIRECT, "1");
  assert.equal(mcp.env.NOVA_TOOLS_READ_ONLY, "1");
  assert.match(options.developerInstructions, /polaris/);
  assert.match(options.developerInstructions, /ponytail:/);
  assert.match(options.developerInstructions, /do not modify files/);
  assert.equal(options.sandbox, "read-only");
  assert.equal(request.parts[0].text, "hello");
  assert.equal(options.baseInstructions, undefined);
  const off = threadOptions(request, { NOVA_FAST_CONTEXT: "0", NOVA_PONYTAIL: "0" });
  assert.equal(off.config["mcp_servers.nova-tools"].enabled, false);
  assert.equal(off.developerInstructions, "");
  assert.equal(off.sandbox, "danger-full-access");
  assert.throws(() => threadOptions(request, {}), /context service/);
  const title = threadOptions({ ...request, action: "title" }, contextEnv);
  assert.equal(title.ephemeral, true);
  assert.equal(title.developerInstructions, "");
  assert.equal(title.config["mcp_servers.nova-tools"].enabled, false);
});

test("app-server stream preserves text, reasoning, tool results, plan and cumulative usage", () => withoutPolaris(async () => {
  const events = [];
  const server = new FakeServer((s) => {
    s.notify("item/started", { item: { id: "a", type: "agentMessage", text: "" } });
    s.notify("item/agentMessage/delta", { itemId: "a", delta: "hello " });
    s.notify("item/agentMessage/delta", { itemId: "a", delta: "world" });
    s.notify("item/completed", { item: { id: "a", type: "agentMessage", text: "hello world", phase: "final_answer" } });
    s.notify("item/reasoning/summaryTextDelta", { itemId: "r", summaryIndex: 0, delta: "thinking" });
    s.notify("item/started", { item: { id: "cmd", type: "commandExecution", command: "echo ok", status: "inProgress" } });
    s.notify("item/commandExecution/outputDelta", { itemId: "cmd", delta: "ok" });
    s.notify("item/completed", { item: { id: "m", type: "mcpToolCall", server: "nova-tools", tool: "polaris", arguments: { query: "Widget" }, status: "completed", result: { content: [{ type: "text", text: "code context" }] }, error: null } });
    s.notify("turn/plan/updated", { plan: [{ step: "verify", status: "inProgress" }] });
    s.notify("thread/tokenUsage/updated", { tokenUsage: { total: { inputTokens: 100, outputTokens: 20, cachedInputTokens: 30 } } });
    s.notify("error", { willRetry: true, error: { message: "Reconnecting 1/5" } });
    s.finish(); // Completion can precede the turn/start response.
  });
  const result = await runTurn(server, request, [], (event) => events.push(event));
  assert.equal(result.type, "done");
  assert.equal(result.usage.input_tokens, 100);
  assert.equal(result.cancelled, false);
  assert.equal(server.closed, true);
  assert.deepEqual(events[0], { type: "ready", sessionId: "thread-1" });
  assert.ok(events.some((e) => e.item?.text === "hello world"));
  assert.ok(events.some((e) => e.item?.text === "thinking"));
  assert.ok(events.some((e) => e.item?.aggregated_output === "ok"));
  const mcp = events.find((e) => e.item?.id === "m").item;
  assert.equal(mcp.type, "mcp_tool_call");
  assert.equal(mcp.result.content[0].text, "code context");
  assert.equal(mcp.error, undefined);
  assert.equal(events.find((e) => e.type === "plan").plan[0].status, "in_progress");
  const turn = server.calls.find(([method]) => method === "turn/start")[1];
  assert.equal(turn.effort, "high");
  assert.deepEqual(turn.input, request.parts);
}));

test("resume refreshes instructions and steering targets the active turn", () => withoutPolaris(async () => {
  const server = new FakeServer();
  await runTurn(server, { ...request, sessionId: "old-thread", mode: "plan" }, [{ action: "steer", parts: [{ type: "text", text: "also check tests" }] }], () => {});
  const resumed = server.calls.find(([method]) => method === "thread/resume")[1];
  assert.equal(resumed.threadId, "old-thread");
  assert.match(resumed.developerInstructions, /read-only/);
  const steer = server.calls.find(([method]) => method === "turn/steer")[1];
  assert.equal(steer.expectedTurnId, "turn-1");
  assert.equal(steer.threadId, "thread-1");
  assert.equal(steer.input[0].text, "also check tests");
}));

test("cancellation received during initialization interrupts the new turn", () => withoutPolaris(async () => {
  const server = new FakeServer();
  const result = await runTurn(server, request, [{ action: "cancel" }], () => {});
  assert.equal(result.cancelled, true);
  assert.deepEqual(server.calls.find(([method]) => method === "turn/interrupt")[1], { threadId: "thread-1", turnId: "turn-1" });
}));

test("failed turns and transport failures surface errors and close the server", () => withoutPolaris(async () => {
  for (const onStart of [s => s.finish("failed", { message: "provider failed" }), s => s.fail(new Error("transport failed"))]) {
    const server = new FakeServer(onStart);
    await assert.rejects(runTurn(server, request, [], () => {}), /failed/);
    assert.equal(server.closed, true);
  }
}));

test("approval replies preserve server IDs and forward the user's decision", () => withoutPolaris(async () => {
  let release;
  const answer = new Promise(resolve => { release = resolve; });
  const controls = (async function* () { yield await answer; })();
  const server = new FakeServer(s => s.onRequest({ id: 42, method: "item/fileChange/requestApproval", params: { reason: "Review patch" } }));
  server.respond = (id, result) => {
    assert.equal(id, 42);
    assert.deepEqual(result, { decision: "decline" });
    server.finish();
  };
  await runTurn(server, request, controls, event => {
    if (event.type === "permission") release({ action: "permission", requestId: event.permission.id, reply: "reject" });
  });
}));

test("image files are removed when a turn fails", () => withoutPolaris(async () => {
  const server = new FakeServer(s => s.finish("failed", { message: "failure after image upload" }));
  await assert.rejects(runTurn(server, { ...request, parts: [{ type: "image_data", data: "aW1hZ2U=" }] }, [], () => {}), /failure after image upload/);
  const input = server.calls.find(([method]) => method === "turn/start")[1].input;
  assert.equal(existsSync(input[0].path), false);
}));

test("title uses ephemeral read-only app-server thread and only returns final text", async () => {
  const server = new FakeServer(s => {
    s.notify("item/completed", { item: { id: "comment", type: "agentMessage", text: "draft", phase: "commentary" } });
    s.notify("item/completed", { item: { id: "final", type: "agentMessage", text: "A short title", phase: "final_answer" } });
    s.finish();
  });
  const events = [];
  const result = await runTurn(server, { ...request, action: "title", prompt: "Make a title" }, [], e => events.push(e));
  assert.deepEqual(result, { ok: true, data: "A short title" });
  assert.deepEqual(events, []);
  assert.equal(server.calls.find(([m]) => m === "thread/start")[1].ephemeral, true);
});

test("fork retains the requested turn boundary and validates it", () => withoutPolaris(async () => {
  const server = new FakeServer();
  assert.equal(await forkThread(server, { ...request, action: "fork", sessionId: "old", retainedTurns: 1 }), "fork-1");
  assert.equal(server.calls.find(([m]) => m === "thread/fork")[1].lastTurnId, "turn-1");
  await assert.rejects(forkThread(new FakeServer(), { ...request, retainedTurns: 3 }), /only has 2 turns/);
}));

test("image inputs use app-server localImage and preserve bytes", async () => {
  const dirs = [];
  try {
    const parts = await inputParts({ parts: [{ type: "image_data", name: "test.png", data: Buffer.from("png bytes").toString("base64") }, { type: "local_image", path: "existing.png" }] }, dirs);
    assert.equal(parts[0].type, "localImage");
    assert.equal(await readFile(parts[0].path, "utf8"), "png bytes");
    assert.equal(parts[1].path, "existing.png");
  } finally { for (const dir of dirs) await rm(dir, { recursive: true, force: true }); }
});

test("file changes preserve diff and MCP errors survive normalization", () => {
  const change = normalizeItem({ id: "f", type: "fileChange", status: "completed", changes: [{ path: "a.txt", kind: { type: "update" }, diff: "+hello" }] });
  assert.equal(change.changes[0].kind, "update");
  assert.equal(change.changes[0].diff, "+hello");
  const tool = normalizeItem({ type: "mcpToolCall", result: null, error: { message: "failed" } });
  assert.equal(tool.result, undefined);
  assert.equal(tool.error.message, "failed");
});

test("stdio transport handshakes, separates server request IDs and rejects pending calls on exit", async () => {
  const script = `
    const rl = require('node:readline').createInterface({input:process.stdin});
    let initialized=false;
    const send=m=>console.log(JSON.stringify(m));
    rl.on('line',line=>{
      const m=JSON.parse(line);
      if(m.method==='initialize') send({id:m.id,result:{}});
      else if(m.method==='initialized') initialized=true;
      else if(m.method==='echo') { if(!initialized) process.exit(2); send({id:m.id,method:'approval',params:{}}); send({id:m.id,result:m.params}); }
      else if(m.method==='exit') process.exit(3);
    });`;
  const server = new AppServer(process.cwd(), { program: process.execPath, args: ["-e", script] });
  try {
    const requests = [];
    server.onRequest = m => { requests.push(m); server.respond(m.id, {}); };
    await server.initialize();
    assert.deepEqual(await server.rpc("echo", { hello: "world" }), { hello: "world" });
    assert.equal(requests.length, 1);
    await assert.rejects(server.rpc("exit", {}), /exited with code 3/);
    await assert.rejects(server.rpc("afterExit", {}), /exited/);
  } finally { await server.close(); }
});

test("real Codex app-server discovers and calls packaged Polaris without a model request", { skip: process.env.NOVA_CODEX_LIVE_TEST !== "1", timeout: 60_000 }, async () => {
  const home = await mkdtemp(join(tmpdir(), "nova-codex-live-"));
  const endpoint = process.platform === "win32" ? `\\\\.\\pipe\\nova-codex-test-${process.pid}` : join(home, "context.sock");
  let received;
  const context = createServer(socket => {
    let data = "";
    socket.on("data", chunk => {
      data += chunk;
      if (!data.includes("\n")) return;
      received = JSON.parse(data.trim());
      socket.end(JSON.stringify({ ok: true, result: "POLARIS_RESULT: Widget definition" }));
    });
  });
  await new Promise(resolve => context.listen(endpoint, resolve));
  const server = new AppServer(request.cwd, { env: { ...process.env, CODEX_HOME: home } });
  try {
    await server.initialize();
    const options = threadOptions(request, { ...contextEnv, NOVA_CONTEXT_SERVICE_ENDPOINT: endpoint });
    const script = resolve("src-tauri/resources/nova-tools-mcp.mjs");
    assert.ok(existsSync(script), "build:nova-tools-mcp must run before the live test");
    options.config["mcp_servers.nova-tools"].args = [script];
    options.ephemeral = true;
    const started = await server.rpc("thread/start", options);
    const threadId = started.thread.id;
    const inventory = await server.rpc("mcpServerStatus/list", { threadId });
    const mcp = inventory.data.find(item => item.name === "nova-tools");
    assert.ok(mcp?.tools.polaris);
    const result = await server.rpc("mcpServer/tool/call", { threadId, server: "nova-tools", tool: "polaris", arguments: { query: "Widget" } });
    assert.match(JSON.stringify(result), /POLARIS_RESULT/);
    assert.equal(received.token, "test-token");
    assert.equal(received.method, "polaris");
    assert.deepEqual(received.params.keywords, ["Widget"]);
    assert.equal(received.root, resolve(request.cwd));
    const title = await server.rpc("thread/start", threadOptions({ ...request, action: "title" }));
    assert.ok(title.thread.id);
  } finally {
    await server.close();
    await new Promise(resolve => context.close(resolve));
    await rm(home, { recursive: true, force: true });
  }
});

test("packaged bridge completes and resumes real app-server turns against a local model stub", { skip: process.env.NOVA_CODEX_LIVE_TEST !== "1", timeout: 60_000 }, async () => {
  const home = await mkdtemp(join(tmpdir(), "nova-codex-turn-"));
  const requests = [];
  const provider = createHttpServer(async (req, res) => {
    if (!req.url.endsWith("/responses")) { res.writeHead(404).end(); return; }
    let body = "";
    for await (const chunk of req) body += chunk;
    requests.push(JSON.parse(body));
    const item = { id: "msg_test", type: "message", role: "assistant", status: "completed", content: [{ type: "output_text", text: "stub response", annotations: [] }] };
    res.writeHead(200, { "Content-Type": "text/event-stream" });
    for (const event of [
      { type: "response.created", response: { id: "resp_test", status: "in_progress", output: [] } },
      { type: "response.output_item.added", output_index: 0, item: { ...item, status: "in_progress", content: [] } },
      { type: "response.output_text.delta", item_id: item.id, output_index: 0, content_index: 0, delta: "stub response" },
      { type: "response.output_item.done", output_index: 0, item },
      { type: "response.completed", response: { id: "resp_test", status: "completed", output: [item], usage: { input_tokens: 12, output_tokens: 3, total_tokens: 15, input_tokens_details: { cached_tokens: 0 } } } },
    ]) res.write(`event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`);
    res.end();
  });
  await new Promise(resolve => provider.listen(0, "127.0.0.1", resolve));
  await writeFile(join(home, "config.toml"), `
model = "test-model"
model_provider = "test"
web_search = "disabled"
[model_providers.test]
name = "Local test"
base_url = "http://127.0.0.1:${provider.address().port}"
wire_api = "responses"
requires_openai_auth = false
`);
  const bridge = async (message) => {
    const child = spawn(process.execPath, [resolve("src-tauri/resources/codex-bridge.mjs")], {
      windowsHide: true,
      env: { ...process.env, CODEX_HOME: home, OPENAI_API_KEY: "", CODEX_API_KEY: "", NOVA_FAST_CONTEXT: "0", NOVA_PONYTAIL: "1" },
      stdio: ["pipe", "pipe", "pipe"],
    });
    let output = "";
    let stderr = "";
    child.stdout.on("data", chunk => { output += chunk; });
    child.stderr.on("data", chunk => { stderr += chunk; });
    const timer = setTimeout(() => child.kill(), 20_000);
    try {
      child.stdin.end(`${JSON.stringify(message)}\n`);
      const code = await new Promise((resolve, reject) => { child.once("close", resolve); child.once("error", reject); });
      assert.equal(code, 0, output + stderr);
      return output.trim().split("\n").map(line => JSON.parse(line));
    } finally { clearTimeout(timer); }
  };
  try {
    const first = await bridge(request);
    const sessionId = first.find(event => event.type === "ready").sessionId;
    assert.ok(first.some(event => event.item?.text === "stub response"));
    assert.equal(first.at(-1).type, "done");
    assert.equal(first.at(-1).usage.input_tokens, 12);
    const second = await bridge({ ...request, sessionId, parts: [{ type: "text", text: "continue" }] });
    assert.equal(second.find(event => event.type === "ready").sessionId, sessionId);
    assert.equal(second.at(-1).usage.input_tokens, 24);
    assert.equal(requests.length, 2);
    assert.match(JSON.stringify(requests[0]), /ponytail:/);
    assert.match(JSON.stringify(requests[1]), /hello/);
    assert.match(JSON.stringify(requests[1]), /continue/);
    const title = await bridge({ ...request, action: "title", prompt: "Make a title" });
    assert.deepEqual(title, [{ ok: true, data: "stub response" }]);
  } finally {
    await new Promise(resolve => provider.close(resolve));
    await rm(home, { recursive: true, force: true });
  }
});
