import { existsSync } from "node:fs";
import { homedir } from "node:os";
import { join, win32 } from "node:path";
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

/** Append Cursor-equivalent batch context policy without replacing CodeBuddy's built-in prompt. */
function codeBuddyBatchToolPolicy(request, env = process.env) {
  const readOnly = request.mode === "plan";
  const fastContext = env.NOVA_FAST_CONTEXT !== "0";
  const tools = fastContext
    ? " plus Nova MCP tools fast_context and find_symbols from server nova-tools"
    : "";
  const lines = [
    `You have CodeBuddy built-in filesystem/search tools${tools}. The following tool-selection rules are hard constraints.`,
    "Prefer minimal reads: when a path and line range are known, read only that segment and expand nearby context only as needed. Do not dump large files blindly.",
    fastContext
      ? "When edit distribution is unknown, or when a task requires understanding two or more unread files, call nova-tools fast_context first; use find_symbols only when definition/reference line numbers are sufficient. One fast_context call typically replaces 5–10 grep+read round-trips. Treat its displayed ranges as already read, and read only coverage gaps or explicitly suggested next locations."
      : "When location is unknown, use a cost-bounded search first and then read only near relevant hits.",
    fastContext
      ? "Do not re-discover the same keywords with Grep, rg, or git grep after fast_context. If fast_context reports CTX MISS, retry once using its next hint or explicit files instead of falling back to repeated searches."
      : "Do not use unscoped recursive grep over a repository or source root.",
    "Do not scan build artifacts, dependencies, caches, generated files, or large binary directories unless the task requires them. Keep edits focused and run the lowest-cost effective validation.",
  ];
  if (readOnly) lines.push("Current mode is plan/read-only: analyze only; do not modify files.");
  return lines.join("\n");
}

/** 为 CodeBuddy 会话挂载 nova-tools MCP（fast_context / find_symbols）；runtime 脚本缺失时不挂载。 */
function novaToolsMcpServers(request, env = process.env) {
  const script = join(env.NOVA_DATA_DIR || join(homedir(), ".nova"), "runtime", "nova-tools-mcp.mjs");
  if (!existsSync(script)) return undefined;
  const serverEnv = {
    NOVA_TOOLS_CWD: request.cwd,
    NOVA_FAST_CONTEXT: env.NOVA_FAST_CONTEXT ?? "1",
  };
  if (request.mode === "plan") serverEnv.NOVA_TOOLS_READ_ONLY = "1";
  for (const key of ["NOVA_CONTEXT_SERVICE_ENDPOINT", "NOVA_CONTEXT_SERVICE_TOKEN"]) {
    if (env[key]) serverEnv[key] = env[key];
  }
  return {
    "nova-tools": { type: "stdio", command: process.execPath, args: [script], env: serverEnv },
  };
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

function assistantItems(message, stream) {
  return (message.message?.content ?? []).flatMap((block, index) => {
    // Text/thinking blocks already seen in stream events must keep the streaming
    // ID. CodeBuddy may use a different message/block ID in the final assistant
    // snapshot; switching IDs would leave both snapshots visible in Nova.
    const streamed = stream?.blocks.has(index) && (block.type === "text" || block.type === "thinking");
    const id = streamed ? `${stream.messageId}-${index}` : block.id ?? `${message.message.id}-${index}`;
    if (block.type === "text") return [{ id, type: "agent_message", text: block.text }];
    if (block.type === "thinking") return [{ id, type: "reasoning", text: block.thinking }];
    if (block.type === "tool_use") return [{ id, type: "mcp_tool_call", server: "CodeBuddy", tool: block.name, arguments: block.input, status: "in_progress" }];
    return [];
  });
}

function emitContent(message, stream) {
  for (const item of assistantItems(message, stream)) send({ type: "item", item });
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
      mcpServers: novaToolsMcpServers(request),
      systemPrompt: { append: codeBuddyBatchToolPolicy(request) },
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
      emitContent(message, stream);
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

function assistantText(message) {
  return (message.message?.content ?? [])
    .filter((block) => block.type === "text")
    .map((block) => block.text)
    .join("");
}

async function generateTitle(request) {
  const cliPath = resolveCodeBuddyCliPath(process.env.NOVA_CODEBUDDY_PATH || undefined);
  let title = "";
  const result = query({
    prompt: request.prompt,
    options: {
      cwd: request.cwd,
      model: request.model || undefined,
      pathToCodebuddyCode: cliPath,
      permissionMode: "bypassPermissions",
      maxTurns: 1,
      stderr: (data) => process.stderr.write(data),
    },
  });
  for await (const message of result) {
    if (message.type === "assistant") title = assistantText(message) || title;
    else if (message.type === "error") throw new Error(message.error);
    else if (message.type === "result" && message.is_error) {
      throw new Error(message.errors?.join("\n") || "CodeBuddy title generation failed");
    }
  }
  return title;
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
    else if (request.action === "title") send({ ok: true, data: await generateTitle(request) });
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

export { assistantItems, assistantText, codeBuddyBatchToolPolicy, novaToolsMcpServers, permissionModeFor, promptMessages, resolveCodeBuddyCliPath, streamEventItem };
