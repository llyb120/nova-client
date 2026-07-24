import { existsSync } from "node:fs";
import { win32 } from "node:path";
import { createInterface } from "node:readline";
import { query, unstable_v2_createSession } from "@tencent-ai/agent-sdk";

function send(message) {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}

function resolveCodeBuddyCliPath(cliPath, fileExists = existsSync) {
  if (!cliPath || !/\.(?:cmd|bat)$/i.test(cliPath)) return cliPath;
  const npmCliPath = win32.join(
    win32.dirname(cliPath),
    "node_modules",
    "@tencent-ai",
    "codebuddy-code",
    "bin",
    "codebuddy",
  );
  return fileExists(npmCliPath) ? npmCliPath : cliPath;
}

function permissionModeFor(mode) {
  return mode === "plan" ? "plan" : "bypassPermissions";
}

async function readRequest(lines) {
  const { value, done } = await lines[Symbol.asyncIterator]().next();
  if (done) throw new Error("Missing request");
  return JSON.parse(value);
}

async function* promptMessages(request) {
  const content = [];
  for (const part of request.parts ?? []) {
    if (part.type === "text") content.push({ type: "text", text: part.text });
    if (part.type === "image_data") content.push({ type: "image", source: { type: "base64", media_type: part.mime, data: part.data } });
    if (part.type === "local_image") {
      const { readFile } = await import("node:fs/promises");
      const { extname } = await import("node:path");
      const mediaTypes = { ".jpg": "image/jpeg", ".jpeg": "image/jpeg", ".png": "image/png", ".gif": "image/gif", ".webp": "image/webp" };
      const mediaType = mediaTypes[extname(part.path).toLowerCase()];
      if (mediaType) content.push({ type: "image", source: { type: "base64", media_type: mediaType, data: (await readFile(part.path)).toString("base64") } });
      else content.push({ type: "text", text: `Attached file: ${part.path}` });
    }
  }
  yield { type: "user", session_id: request.sessionId || "", message: { role: "user", content }, parent_tool_use_id: null };
}

function assistantItems(message) {
  return (message.message?.content ?? []).flatMap((block, index) => {
    // Match the streaming item ID so the complete assistant message is the
    // authoritative final snapshot, even when partial events were dropped.
    const id = block.id ?? `${message.message.id}-${index}`;
    if (block.type === "text") return [{ id, type: "agent_message", text: block.text }];
    if (block.type === "thinking") return [{ id, type: "reasoning", text: block.thinking }];
    if (block.type === "tool_use") return [{ id, type: "mcp_tool_call", server: "CodeBuddy", tool: block.name, arguments: block.input, status: "in_progress" }];
    return [];
  });
}

function emitContent(message) {
  for (const item of assistantItems(message)) send({ type: "item", item });
}

function streamEventItem(message, stream) {
  const event = message.event;
  if (event.type === "message_start") {
    stream.messageId = event.message.id;
    stream.blocks.clear();
    return null;
  }
  if (event.type === "content_block_start") {
    const block = event.content_block;
    if (block.type !== "text" && block.type !== "thinking") return null;
    const text = block.type === "text" ? block.text : block.thinking;
    stream.blocks.set(event.index, { type: block.type, text });
    if (!text) return null;
    return {
      id: `${stream.messageId}-${event.index}`,
      type: block.type === "text" ? "agent_message" : "reasoning",
      text,
    };
  }
  if (event.type !== "content_block_delta") return null;
  const delta = event.delta;
  if (delta.type !== "text_delta" && delta.type !== "thinking_delta") return null;
  const block = stream.blocks.get(event.index) ?? { type: delta.type === "text_delta" ? "text" : "thinking", text: "" };
  block.text += delta.type === "text_delta" ? delta.text : delta.thinking;
  stream.blocks.set(event.index, block);
  return {
    id: `${stream.messageId}-${event.index}`,
    type: block.type === "text" ? "agent_message" : "reasoning",
    text: block.text,
  };
}

async function runPrompt(lines, request) {
  const pending = new Map();
  const stream = { messageId: "message", blocks: new Map() };
  let sessionId = request.sessionId;
  let checkpoint;
  let activeQuery;
  const cliPath = resolveCodeBuddyCliPath(process.env.NOVA_CODEBUDDY_PATH || undefined);
  const input = (async () => {
    for await (const line of lines) {
      if (!line.trim()) continue;
      const command = JSON.parse(line);
      if (command.action === "cancel") await activeQuery?.interrupt();
      if (command.action === "permission") {
        const resolve = pending.get(command.requestId);
        if (resolve) {
          pending.delete(command.requestId);
          resolve(command.reply === "reject"
            ? { behavior: "deny", message: "Rejected by user" }
            : { behavior: "allow" });
        }
      }
    }
  })();
  activeQuery = query({
    prompt: promptMessages(request),
    options: {
      cwd: request.cwd,
      resume: request.sessionId || undefined,
      resumeSessionAt: request.restoreAt || undefined,
      forkSession: Boolean(request.restoreAt),
      model: request.model || undefined,
      effort: request.reasoningEffort || undefined,
      includePartialMessages: true,
      pathToCodebuddyCode: cliPath,
      stderr: (data) => process.stderr.write(data),
      permissionMode: permissionModeFor(request.mode),
      canUseTool: (tool, toolInput, options) => new Promise((resolve) => {
        pending.set(options.toolUseID, resolve);
        send({ type: "permission", permission: { id: options.toolUseID, permission: tool, metadata: toolInput } });
      }),
    },
  });
  for await (const message of activeQuery) {
    if (message.type === "system" && message.subtype === "init") {
      sessionId = message.session_id;
      send({ type: "ready", sessionId });
    }
    else if (message.type === "stream_event") {
      const item = streamEventItem(message, stream);
      if (item) send({ type: "item", item });
    }
    else if (message.type === "assistant") {
      checkpoint = message.uuid;
      emitContent(message);
    }
    else if (message.type === "error") throw new Error(message.error);
    else if (message.type === "result") {
      if (message.is_error) throw new Error(message.errors?.join("\n") || "CodeBuddy turn failed");
      if (sessionId && checkpoint) send({ type: "checkpoint", sessionId, position: checkpoint });
      send({ type: "done", usage: message.usage });
    }
  }
  void input;
}

async function modelOptions(request) {
  const cliPath = resolveCodeBuddyCliPath(process.env.NOVA_CODEBUDDY_PATH || undefined);
  if (cliPath) process.env.CODEBUDDY_CODE_PATH = cliPath;
  const session = unstable_v2_createSession({
    cwd: request.cwd,
    pathToCodebuddyCode: cliPath,
  });
  try {
    const models = await session.getAvailableModelsRaw();
    return {
      configOptions: [{
        id: "model",
        name: "Model",
        currentValue: "",
        options: models.map((model) => ({
          value: model.id,
          name: model.name ?? model.id,
          description: model.credits ?? model.description,
          _meta: { "codex.ai/supportsImages": model.supportsImages ?? false },
        })),
      }],
      modes: null,
    };
  } finally {
    session.close();
  }
}

async function main() {
  const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
  let request;
  try {
    request = await readRequest(lines);
    if (request.action === "prompt") await runPrompt(lines, request);
    else if (request.action === "models") send({ ok: true, data: await modelOptions(request) });
    else throw new Error(`Unknown action: ${request.action}`);
  } catch (error) {
    send({ ok: false, error: error instanceof Error ? error.message : String(error) });
    process.exitCode = 1;
  } finally {
    lines.close();
    if (request?.action === "models") process.exit(0);
  }
}

if (process.env.NOVA_CODEBUDDY_BRIDGE_TEST !== "1") void main();

export { assistantItems, permissionModeFor, promptMessages, resolveCodeBuddyCliPath, streamEventItem };
