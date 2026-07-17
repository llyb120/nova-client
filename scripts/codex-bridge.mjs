import { createInterface } from "node:readline";
import { spawn } from "node:child_process";
import { Codex } from "@openai/codex-sdk";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, dirname, extname, isAbsolute, join } from "node:path";

function send(message) {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}

function codexPathOverride() {
  const path = process.env.NOVA_CODEX_PATH;
  if (process.platform === "win32" && path && !path.toLowerCase().endsWith(".exe")) {
    const target = process.arch === "arm64" ? "aarch64-pc-windows-msvc" : "x86_64-pc-windows-msvc";
    const packageName = process.arch === "arm64" ? "codex-win32-arm64" : "codex-win32-x64";
    const roots = new Set();
    if (isAbsolute(path)) roots.add(dirname(path));
    if (process.env.APPDATA) roots.add(join(process.env.APPDATA, "npm"));
    if (process.env.USERPROFILE) roots.add(join(process.env.USERPROFILE, "AppData", "Roaming", "npm"));
    if (process.env.npm_config_prefix) roots.add(process.env.npm_config_prefix);
    for (const root of (process.env.PATH ?? "").split(delimiter)) {
      if (root && (existsSync(join(root, "codex.cmd")) || existsSync(join(root, "codex.ps1")))) roots.add(root);
    }
    const candidates = [...roots].flatMap((npmRoot) => [
      join(npmRoot, "node_modules", "@openai", "codex", "node_modules", "@openai", packageName, "vendor", target, "bin", "codex.exe"),
      join(npmRoot, "node_modules", "@openai", packageName, "vendor", target, "bin", "codex.exe"),
    ]);
    return candidates.find(existsSync);
  }
  return path || undefined;
}

async function readRequest(lines) {
  const { value, done } = await lines[Symbol.asyncIterator]().next();
  if (done) throw new Error("Missing request");
  return JSON.parse(value);
}

function threadOptions(request) {
  const build = request.mode !== "plan";
  return {
    workingDirectory: request.cwd,
    skipGitRepoCheck: true,
    model: request.model || undefined,
    modelReasoningEffort: request.reasoningEffort || undefined,
    sandboxMode: build ? "danger-full-access" : "read-only",
    approvalPolicy: "never",
  };
}

async function inputParts(request) {
  const parts = [];
  let imageDir;
  for (const part of request.parts ?? []) {
    if (part.type === "text") parts.push({ type: "text", text: part.text });
    if (part.type === "local_image") parts.push(part);
    if (part.type === "image_data") {
      imageDir ??= await mkdtemp(join(tmpdir(), "nova-codex-"));
      const suffix = extname(part.name || "") || `.${part.mime.split("/").at(-1) || "png"}`;
      const path = join(imageDir, `${parts.length}${suffix}`);
      await writeFile(path, Buffer.from(part.data, "base64"));
      parts.push({ type: "local_image", path });
    }
  }
  return { parts, imageDir };
}

async function runPrompt(codex, lines, request) {
  const controller = new AbortController();
  const input = (async () => {
    for await (const line of lines) {
      if (!line.trim()) continue;
      if (JSON.parse(line).action === "cancel") controller.abort();
    }
  })();
  const options = threadOptions(request);
  const thread = request.sessionId
    ? codex.resumeThread(request.sessionId, options)
    : codex.startThread(options);
  const { parts, imageDir } = await inputParts(request);
  try {
    const { events } = await thread.runStreamed(parts, { signal: controller.signal });
    for await (const event of events) {
      if (event.type === "thread.started") send({ type: "ready", sessionId: event.thread_id });
      else if (event.type === "item.started" || event.type === "item.updated" || event.type === "item.completed") {
        send({ type: "item", item: event.item });
      } else if (event.type === "turn.completed") send({ type: "done", usage: event.usage });
      else if (event.type === "turn.failed") throw new Error(event.error.message);
      else if (event.type === "error") throw new Error(event.message);
    }
  } finally {
    if (imageDir) await rm(imageDir, { recursive: true, force: true });
  }
  void input;
}

async function generateTitle(codex, request) {
  const thread = codex.startThread({
    workingDirectory: request.cwd,
    skipGitRepoCheck: true,
    model: request.model || undefined,
    sandboxMode: "read-only",
    approvalPolicy: "never",
  });
  return (await thread.run(request.prompt)).finalResponse;
}

async function forkThread(request) {
  const child = spawn(codexPathOverride() ?? "codex", ["app-server"], {
    cwd: request.cwd,
    windowsHide: true,
    stdio: ["pipe", "pipe", "ignore"],
  });
  const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
  const pending = new Map();
  let nextId = 1;
  const exited = new Promise((_, reject) => {
    child.once("error", reject);
    child.once("exit", (code) => reject(new Error(`Codex app-server exited with code ${code}`)));
  });
  void (async () => {
    for await (const line of lines) {
      if (!line.trim()) continue;
      const message = JSON.parse(line);
      if (message.id == null) continue;
      const callback = pending.get(message.id);
      if (!callback) continue;
      pending.delete(message.id);
      if (message.error) callback.reject(new Error(message.error.message ?? JSON.stringify(message.error)));
      else callback.resolve(message.result);
    }
  })();
  function rpc(method, params) {
    const id = nextId++;
    const response = new Promise((resolve, reject) => pending.set(id, { resolve, reject }));
    child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
    return Promise.race([response, exited]);
  }
  try {
    await rpc("initialize", {
      clientInfo: { name: "nova-sdk-bridge", title: "Nova", version: "0.1.0" },
      capabilities: { experimentalApi: true },
    });
    const read = await rpc("thread/read", {
      threadId: request.sessionId,
      includeTurns: true,
    });
    const turns = read.thread?.turns ?? [];
    const lastTurn = turns[request.retainedTurns - 1];
    if (!lastTurn) throw new Error(`Codex session only has ${turns.length} turns`);
    const fork = await rpc("thread/fork", {
      threadId: request.sessionId,
      lastTurnId: lastTurn.id,
      cwd: request.cwd,
    });
    return fork.thread.id;
  } finally {
    lines.close();
    child.kill();
  }
}

async function main() {
  const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
  try {
    const request = await readRequest(lines);
    const codex = new Codex({ codexPathOverride: codexPathOverride() });
    if (request.action === "prompt") await runPrompt(codex, lines, request);
    else if (request.action === "title") send({ ok: true, data: await generateTitle(codex, request) });
    else if (request.action === "fork") send({ ok: true, data: await forkThread(request) });
    else throw new Error(`Unknown action: ${request.action}`);
  } catch (error) {
    send({ ok: false, error: error instanceof Error ? error.message : String(error) });
    process.exitCode = 1;
  } finally {
    lines.close();
  }
}

void main();
