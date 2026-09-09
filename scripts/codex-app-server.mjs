import { createInterface } from "node:readline";
import { spawn } from "node:child_process";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, dirname, extname, isAbsolute, join } from "node:path";
import { fileURLToPath } from "node:url";
import { ponytailPrompt } from "./ponytail-prompt.mjs";

const send = (message) => process.stdout.write(`${JSON.stringify(message)}\n`);

export function codexPathOverride(env = process.env) {
  const path = env.NOVA_CODEX_PATH || "codex";
  if (process.platform !== "win32" || path.toLowerCase().endsWith(".exe")) return path;
  const target = process.arch === "arm64" ? "aarch64-pc-windows-msvc" : "x86_64-pc-windows-msvc";
  const packageName = process.arch === "arm64" ? "codex-win32-arm64" : "codex-win32-x64";
  const roots = new Set();
  if (isAbsolute(path)) roots.add(dirname(path));
  if (env.APPDATA) roots.add(join(env.APPDATA, "npm"));
  if (env.npm_config_prefix) roots.add(env.npm_config_prefix);
  for (const root of (env.PATH ?? "").split(delimiter)) {
    if (root && (existsSync(join(root, "codex.cmd")) || existsSync(join(root, "codex.ps1")))) roots.add(root);
  }
  for (const root of roots) {
    const binary = [
      join(root, "node_modules", "@openai", "codex", "node_modules", "@openai", packageName, "vendor", target, "bin", "codex.exe"),
      join(root, "node_modules", "@openai", packageName, "vendor", target, "bin", "codex.exe"),
      join(root, "node_modules", "@openai", "codex", "vendor", target, "bin", "codex.exe"),
    ].find(existsSync);
    if (binary) return binary;
  }
  throw new Error(`Cannot resolve Codex executable from ${path}; configure the native codex.exe path`);
}

// Inherit proxy settings, isolated CODEX_HOME and the host's Windows shell shim.
export class AppServer {
  constructor(cwd, { program = codexPathOverride(), args = ["app-server"], env = process.env } = {}) {
    this.pending = new Map();
    this.nextId = 1;
    this.onNotification = () => {};
    this.onRequest = (message) => this.respond(message.id, undefined, {
      code: -32601, message: `Unsupported client request: ${message.method}`,
    });
    this.failure = new Promise((_, reject) => { this.rejectFailure = reject; });
    this.failure.catch(() => {});
    this.child = spawn(program, args, { cwd, env, windowsHide: true, stdio: ["pipe", "pipe", "pipe"] });
    this.child.stderr.on("data", (data) => process.stderr.write(data));
    this.child.on("error", (error) => this.fail(error));
    this.child.stdin.on("error", (error) => this.fail(error));
    this.exited = new Promise((resolve) => this.child.once("close", (code) => {
      this.fail(new Error(`Codex app-server exited with code ${code}`));
      resolve();
    }));
    this.lines = createInterface({ input: this.child.stdout, crlfDelay: Infinity });
    this.lines.on("line", (line) => {
      if (!line.trim()) return;
      try {
        const message = JSON.parse(line);
        // Server request IDs are independent of client request IDs.
        if (message.method) {
          Promise.resolve(message.id == null ? this.onNotification(message) : this.onRequest(message))
            .catch((error) => this.fail(error));
        } else {
          const pending = this.pending.get(message.id);
          if (!pending) return;
          this.pending.delete(message.id);
          clearTimeout(pending.timer);
          if (message.error) pending.reject(new Error(message.error.message || JSON.stringify(message.error)));
          else pending.resolve(message.result);
        }
      } catch (error) { this.fail(error); }
    });
  }

  fail(error) {
    this.error ??= error;
    this.rejectFailure(error);
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.pending.clear();
  }

  write(message) {
    if (this.error) throw this.error;
    this.child.stdin.write(`${JSON.stringify(message)}\n`);
  }

  rpc(method, params, timeoutMs = 120_000) {
    if (this.error) return Promise.reject(this.error);
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`Codex app-server request timed out: ${method}`));
      }, timeoutMs);
      this.pending.set(id, { resolve, reject, timer });
      try { this.write({ id, method, params }); }
      catch (error) { this.fail(error); }
    });
  }

  respond(id, result, error) { this.write(error ? { id, error } : { id, result }); }

  async initialize() {
    await this.rpc("initialize", {
      clientInfo: { name: "nova_app_server", title: "Nova", version: "0.1.0" },
      capabilities: { experimentalApi: true },
    });
    this.write({ method: "initialized", params: {} });
  }

  async close() {
    // EOF lets Codex stop MCP children before Nova reaps the bridge.
    this.child.stdin.end();
    const timer = setTimeout(() => {
      if (this.child.exitCode !== null || !this.child.pid) return;
      if (process.platform === "win32") {
        const killer = spawn("taskkill", ["/PID", String(this.child.pid), "/T", "/F"], { windowsHide: true, stdio: "ignore" });
        killer.on("error", () => this.child.kill());
      } else this.child.kill();
    }, 1500);
    try { await this.exited; }
    finally { clearTimeout(timer); this.lines.close(); }
  }
}

export function threadOptions(request, env = process.env) {
  const title = request.action === "title";
  const readOnly = title || request.mode === "plan";
  const polaris = !title && env.NOVA_FAST_CONTEXT !== "0" && env.NOVA_CONTEXT_RETRIEVAL_MODE !== "none";
  const config = {};
  const guidance = [];
  if (polaris) {
    if (!env.NOVA_CONTEXT_SERVICE_ENDPOINT || !env.NOVA_CONTEXT_SERVICE_TOKEN) {
      throw new Error("Polaris is enabled but the Nova context service is not configured");
    }
    const script = join(dirname(fileURLToPath(import.meta.url)), "nova-tools-mcp.mjs");
    if (!existsSync(script)) throw new Error(`Missing Polaris MCP server: ${script}`);
    config["mcp_servers.nova-tools"] = {
      command: process.execPath, args: [script], required: true, enabled: true,
      env: {
        NOVA_TOOLS_CWD: request.cwd,
        NOVA_FAST_CONTEXT: "1", NOVA_CONTEXT_RETRIEVAL_MODE: "fast", NOVA_MCP_DIRECT: "1",
        NOVA_TOOLS_READ_ONLY: readOnly ? "1" : "0", NOVA_BROWSER_DEBUG: "0",
        NOVA_CONTEXT_SERVICE_ENDPOINT: env.NOVA_CONTEXT_SERVICE_ENDPOINT,
        NOVA_CONTEXT_SERVICE_TOKEN: env.NOVA_CONTEXT_SERVICE_TOKEN,
      },
    };
    guidance.push("Nova provides the polaris tool through MCP server nova-tools. Call its exposed MCP tool directly, using the name in the tool schema; do not use Devin's mcp_call_tool wrapper. When edit locations are unknown or you need to read two or more unread files, call polaris first with the user's symbols, keywords or file paths. It returns complete code units, dependencies and coverage gaps. Do not re-read covered ranges or rediscover the same keywords with shell searches; read only gaps / next_reads by exact path and line. Keep searches bounded and honor .gitignore. Use Codex's built-in file and shell tools for edits and verification.");
  } else {
    // A disabled entry still needs a valid transport in Codex's config schema.
    config["mcp_servers.nova-tools"] = { command: process.execPath, args: [], enabled: false };
  }
  if (!title) {
    guidance.push(ponytailPrompt(env));
    if (readOnly) guidance.push("Current Nova mode is plan/read-only: analyze and propose a plan; do not modify files.");
  }
  return {
    cwd: request.cwd, model: request.model || undefined,
    sandbox: readOnly ? "read-only" : "danger-full-access", approvalPolicy: "never",
    config, developerInstructions: guidance.filter(Boolean).join("\n\n"),
    ...(title ? { ephemeral: true } : {}),
  };
}

export async function inputParts(request, imageDirs) {
  const parts = [];
  for (const part of request.parts ?? []) {
    if (part.type === "text") parts.push({ type: "text", text: part.text });
    else if (part.type === "local_image") parts.push({ type: "localImage", path: part.path });
    else if (part.type === "image_data") {
      const dir = await mkdtemp(join(tmpdir(), "nova-codex-"));
      imageDirs.push(dir);
      const suffix = extname(part.name || "") || ".png";
      const path = join(dir, `image${suffix}`);
      await writeFile(path, Buffer.from(part.data, "base64"));
      parts.push({ type: "localImage", path });
    }
  }
  return parts;
}

const status = (value) => value === "inProgress" ? "in_progress" : value;

// Nova consumes snapshots; its shared runtime converts appended text to UI deltas.
export function normalizeItem(item) {
  switch (item.type) {
    case "agentMessage": case "plan": return { id: item.id, type: "agent_message", text: item.text ?? "" };
    case "reasoning": return { id: item.id, type: "reasoning", text: (item.summary?.length ? item.summary : item.content ?? []).join("\n") };
    case "commandExecution": return { ...item, type: "command_execution", status: status(item.status), aggregated_output: item.aggregatedOutput ?? "", exit_code: item.exitCode };
    case "fileChange": return { ...item, type: "file_change", status: status(item.status), changes: (item.changes ?? []).map((change) => ({ ...change, kind: change.kind?.type ?? change.kind })) };
    case "mcpToolCall": return { ...item, type: "mcp_tool_call", status: status(item.status), result: item.result ?? undefined, error: item.error ?? undefined };
    case "dynamicToolCall": return { ...item, type: "mcp_tool_call", server: "Codex", status: status(item.status), result: { content: item.contentItems } };
    case "webSearch": return { ...item, type: "web_search", status: status(item.status) };
    default: return null;
  }
}

export async function runTurn(server, request, controls, emit = send) {
  const title = request.action === "title";
  const items = new Map();
  const imageDirs = [];
  const permissions = new Map();
  let threadId;
  let turnId;
  let usage;
  let finished = false;
  let resolveTurn;
  const completed = new Promise((resolve) => { resolveTurn = resolve; });
  let releaseControls;
  const turnReady = new Promise((resolve) => { releaseControls = resolve; });
  const emitItem = (item) => {
    items.set(item.id, item);
    const normalized = normalizeItem(item);
    if (!title && normalized) emit({ type: "item", item: normalized });
  };
  server.onNotification = ({ method, params: p }) => {
    if (p.threadId && p.threadId !== threadId) return;
    if (p.turnId && turnId && p.turnId !== turnId) return;
    if (method === "turn/started") { turnId = p.turn.id; releaseControls(); }
    else if (method === "item/started" || method === "item/completed") emitItem(p.item);
    else if (method === "item/agentMessage/delta" || method === "item/plan/delta") {
      const item = items.get(p.itemId) ?? { id: p.itemId, type: "agentMessage", text: "" };
      emitItem({ ...item, text: item.text + p.delta });
    } else if (method === "item/reasoning/summaryTextDelta" || method === "item/reasoning/textDelta") {
      const item = items.get(p.itemId) ?? { id: p.itemId, type: "reasoning", summary: [], content: [] };
      const field = method.includes("summary") ? "summary" : "content";
      const chunks = [...(item[field] ?? [])];
      const index = p.summaryIndex ?? p.contentIndex ?? 0;
      chunks[index] = (chunks[index] ?? "") + p.delta;
      emitItem({ ...item, [field]: chunks });
    } else if (method === "item/commandExecution/outputDelta") {
      const item = items.get(p.itemId);
      if (item) emitItem({ ...item, aggregatedOutput: (item.aggregatedOutput ?? "") + p.delta });
    } else if (method === "turn/plan/updated" && !title) {
      emit({ type: "plan", plan: p.plan.map((step) => ({ content: step.step, status: status(step.status), priority: "medium" })) });
    } else if (method === "thread/tokenUsage/updated") {
      const total = p.tokenUsage.total;
      usage = { input_tokens: total.inputTokens, output_tokens: total.outputTokens, cached_input_tokens: total.cachedInputTokens, cache_creation_input_tokens: total.cacheWriteInputTokens ?? 0 };
    } else if (method === "turn/completed") { finished = true; resolveTurn(p.turn); }
    else if (method === "error" && !p.willRetry) server.fail(new Error(p.error?.message || "Codex turn failed"));
  };
  server.onRequest = (message) => {
    if (["item/commandExecution/requestApproval", "item/fileChange/requestApproval"].includes(message.method)) {
      if (title) { server.respond(message.id, { decision: "decline" }); return; }
      const key = String(message.id);
      permissions.set(key, message.id);
      emit({ type: "permission", permission: { id: key, permission: message.params.reason || message.method, metadata: message.params } });
    } else server.respond(message.id, undefined, { code: -32601, message: `Nova does not support ${message.method}` });
  };
  // Start reading before initialization so early cancellation and steering are retained.
  const controlTask = (async () => {
    for await (const control of controls) {
      if (finished) break;
      if (control.action === "permission") {
        const id = permissions.get(control.requestId);
        if (id !== undefined) {
          permissions.delete(control.requestId);
          server.respond(id, { decision: control.reply === "reject" ? "decline" : "accept" });
        }
      } else if (control.action === "cancel" || control.action === "steer") {
        await Promise.race([turnReady, server.failure]);
        if (finished) break;
        if (control.action === "cancel") await server.rpc("turn/interrupt", { threadId, turnId });
        else {
          try {
            await server.rpc("turn/steer", { threadId, expectedTurnId: turnId, input: await inputParts(control, imageDirs) });
          } catch (error) {
            if (!title) emit({ type: "item", item: { id: `steer-error-${Date.now()}`, type: "error", message: `会话引导失败：${error.message}` } });
          }
        }
      }
    }
  })();
  controlTask.catch((error) => { if (!finished) server.fail(error); });
  try {
    await server.initialize();
    const options = threadOptions(request);
    const response = await server.rpc(request.sessionId ? "thread/resume" : "thread/start", {
      ...options, ...(request.sessionId ? { threadId: request.sessionId } : {}),
    });
    threadId = response.thread.id;
    if (!title) emit({ type: "ready", sessionId: threadId });
    const input = title ? [{ type: "text", text: request.prompt }] : await inputParts(request, imageDirs);
    const started = await server.rpc("turn/start", {
      threadId, input, cwd: request.cwd, model: request.model || undefined,
      effort: request.reasoningEffort || undefined, approvalPolicy: "never",
      sandboxPolicy: { type: title || request.mode === "plan" ? "readOnly" : "dangerFullAccess" },
    });
    turnId = started.turn.id;
    releaseControls();
    const turn = await Promise.race([completed, server.failure]);
    if (turn.status === "failed") throw new Error(turn.error?.message || "Codex turn failed");
    if (title) {
      if (turn.status === "interrupted") throw new Error("Codex title generation interrupted");
      const messages = [...items.values()].filter((item) => item.type === "agentMessage");
      return { ok: true, data: messages.filter((item) => item.phase === "final_answer").map((item) => item.text).join("\n") || messages.at(-1)?.text || "" };
    }
    return { type: "done", usage, cancelled: turn.status === "interrupted" };
  } finally {
    finished = true;
    releaseControls();
    await server.close();
    for (const dir of imageDirs) await rm(dir, { recursive: true, force: true });
  }
}

export async function forkThread(server, request) {
  try {
    if (!Number.isInteger(request.retainedTurns) || request.retainedTurns < 1) throw new Error("retainedTurns must be a positive integer");
    await server.initialize();
    const read = await server.rpc("thread/read", { threadId: request.sessionId, includeTurns: true });
    const turns = read.thread?.turns ?? [];
    const lastTurn = turns[request.retainedTurns - 1];
    if (!lastTurn) throw new Error(`Codex session only has ${turns.length} turns`);
    const fork = await server.rpc("thread/fork", {
      ...threadOptions(request), threadId: request.sessionId, lastTurnId: lastTurn.id,
    });
    return fork.thread.id;
  } finally { await server.close(); }
}

export async function main() {
  const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
  const iterator = lines[Symbol.asyncIterator]();
  try {
    const first = await iterator.next();
    if (first.done) throw new Error("Missing request");
    const request = JSON.parse(first.value);
    if (!["prompt", "title", "fork"].includes(request.action)) throw new Error(`Unknown action: ${request.action}`);
    const controls = (async function* () {
      for await (const line of { [Symbol.asyncIterator]: () => iterator }) {
        if (line.trim()) yield JSON.parse(line);
      }
    })();
    const server = new AppServer(request.cwd);
    if (request.action === "fork") send({ ok: true, data: await forkThread(server, request) });
    else send(await runTurn(server, request, controls));
  } catch (error) {
    send({ ok: false, error: error instanceof Error ? error.message : String(error) });
    process.exitCode = 1;
  } finally { lines.close(); }
}
