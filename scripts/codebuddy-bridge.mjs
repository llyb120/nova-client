import { existsSync } from "node:fs";
import { win32 } from "node:path";
import { createInterface } from "node:readline";
import { createSdkMcpServer, query, tool, unstable_v2_createSession } from "@tencent-ai/agent-sdk";
import { z } from "zod";
import { createNovaBatchTools } from "./nova-batch-tools.mjs";

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

const CODEBUDDY_USAGE_KEYS = [
  "input_tokens",
  "output_tokens",
  "cache_read_input_tokens",
  "cache_creation_input_tokens",
];

function mergeCodeBuddyUsage(total, usage) {
  if (!usage || typeof usage !== "object") return total;
  const next = { ...(total ?? {}) };
  let found = false;
  for (const key of CODEBUDDY_USAGE_KEYS) {
    const value = Number(usage[key]);
    if (!Number.isFinite(value) || value < 0) continue;
    next[key] = (Number(next[key]) || 0) + value;
    found = true;
  }
  return found ? next : total;
}

function permissionModeFor(mode) {
  return mode === "plan" ? "plan" : "bypassPermissions";
}

/** Append Cursor-equivalent batch context policy without replacing CodeBuddy's built-in prompt. */
function codeBuddyBatchToolPolicy(request, env = process.env) {
  const readOnly = request.mode === "plan";
  const fastContext = env.NOVA_FAST_CONTEXT !== "0";
  const tools = fastContext
    ? " plus Nova MCP tool polaris from server nova-tools"
    : "";
  const lines = [
    `You have CodeBuddy built-in filesystem/search tools${tools}. The following tool-selection rules are hard constraints.`,
    "Prefer minimal reads: when a path and line range are known, read only that segment and expand nearby context only as needed. Do not dump large files blindly.",
    fastContext
      ? "When edit distribution is unknown, or when a task requires understanding two or more unread files, call nova-tools polaris first. One polaris call typically replaces 5–10 grep+read round-trips. Treat its displayed ranges as already read, and read only coverage gaps or explicitly suggested next locations."
      : "When location is unknown, use a cost-bounded search first and then read only near relevant hits.",
    fastContext
      ? "Do not re-discover the same keywords with Grep, rg, or git grep after polaris. If polaris reports CTX MISS, retry once using its next hint or explicit files instead of falling back to repeated searches."
      : "Do not use unscoped recursive grep over a repository or source root.",
    "Do not scan build artifacts, dependencies, caches, generated files, or large binary directories unless the task requires them. Keep edits focused and run the lowest-cost effective validation.",
  ];
  if (readOnly) lines.push("Current mode is plan/read-only: analyze only; do not modify files.");
  return lines.join("\n");
}

/**
 * Register Nova tools as an in-process SDK MCP server. CodeBuddy's CLI can fail to discover a
 * spawned stdio sidecar even when that sidecar itself is healthy; SDK MCP uses the agent-sdk
 * control channel and therefore appears in the CLI tool registry before the first model turn.
 */
function novaToolsMcpServers(request, env = process.env, createServer = createSdkMcpServer) {
  if (env.NOVA_FAST_CONTEXT === "0") return undefined;
  const batchTools = createNovaBatchTools(request.cwd, {
    fastContext: true,
    readOnly: request.mode === "plan",
  });
  const polaris = batchTools.polaris;
  return {
    "nova-tools": createServer({
      name: "nova-tools",
      version: "1.0.0",
      tools: [
        tool("polaris", polaris.description, {
          keywords: z.array(z.string().min(1)).min(1).max(5).optional(),
          query: z.string().min(1).optional(),
          task: z.string().min(1).optional(),
          files: z.array(z.string().min(1)).min(1).max(6).optional(),
          budget: z.number().int().min(100).max(4000).optional(),
          maxChars: z.number().int().min(4000).max(80000).optional(),
          coupling: z.boolean().optional(),
        }, async (args) => ({ content: [{ type: "text", text: await polaris.execute(args) }] })),
      ],
    }),
  };
}

async function readRequest(lines) {
  const { value, done } = await lines[Symbol.asyncIterator]().next();
  if (done) throw new Error("Missing request");
  return JSON.parse(value);
}

async function* promptMessages(request, env = process.env) {
  const content = [];
  const policy = codeBuddyBatchToolPolicy(request, env);
  // CodeBuddy may retain the original system prompt when resuming a CLI session. Cursor avoids
  // that problem by attaching its policy to every user message; do the same here so the current
  // turn always sees the MCP routing rule immediately before the user's request.
  if (policy) content.push({
    type: "text",
    text: `${policy}${env.NOVA_FAST_CONTEXT !== "0"
      ? "\n\nCURRENT TURN ROUTING: If repository discovery/search is needed, your first repository tool call MUST be mcp__nova-tools__polaris (the polaris tool from MCP server nova-tools). Do not call Grep, Glob, Search, or Bash search first. Only use built-in search after polaris when its output explicitly leaves a coverage gap."
      : ""}\n\nUser request follows:`,
  });
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

const TRAILING_PUNCTUATION_RE = /^[\p{P}\p{S}]+$/u;

function assistantItems(message, stream) {
  const matchedStreamIndexes = new Set();
  const finalizedBlocks = stream?.finalizedBlocks ?? [];
  const items = (message.message?.content ?? []).flatMap((block, index) => {
    let streamedIndex;
    let alreadyStreamedSuffix = false;
    if (block.type === "text" || block.type === "thinking") {
      const finalText = block.type === "text" ? block.text : block.thinking;
      // CodeBuddy may emit the main answer and its final punctuation as two
      // assistant snapshots. The second snapshot has no matching block because
      // message_start cleared the current stream, so also compare with earlier
      // finalized snapshots from this turn.
      if (
        finalText &&
        TRAILING_PUNCTUATION_RE.test(finalText) &&
        finalizedBlocks.some((candidate) => candidate.type === block.type && candidate.text?.endsWith(finalText))
      ) {
        alreadyStreamedSuffix = true;
      }
      // The final snapshot may omit or reorder thinking blocks, so its array
      // indexes do not necessarily match content_block event indexes. Match the
      // accumulated stream by type/content and retain its ID.
      for (const [candidateIndex, candidate] of stream?.blocks ?? []) {
        if (alreadyStreamedSuffix) break;
        if (matchedStreamIndexes.has(candidateIndex) || candidate.type !== block.type) continue;
        if (finalText === candidate.text || finalText?.startsWith(candidate.text)) {
          streamedIndex = candidateIndex;
          matchedStreamIndexes.add(candidateIndex);
          break;
        }
        // Some CodeBuddy SDK versions yield a final assistant snapshot containing
        // only the last delta (often a trailing punctuation mark). It was already
        // included in the accumulated stream, so emitting it with the snapshot ID
        // would create a standalone duplicate message.
        if (finalText && TRAILING_PUNCTUATION_RE.test(finalText) && candidate.text?.endsWith(finalText)) {
          streamedIndex = candidateIndex;
          alreadyStreamedSuffix = true;
          matchedStreamIndexes.add(candidateIndex);
          break;
        }
      }
      if (!alreadyStreamedSuffix) {
        stream?.finalizedBlocks?.push({ type: block.type, text: finalText });
      }
    }
    if (alreadyStreamedSuffix) return [];
    const id = streamedIndex !== undefined
      ? `${stream.messageId}-${streamedIndex}`
      : block.id ?? `${message.message.id}-${index}`;
    if (block.type === "text") return [{ id, type: "agent_message", text: block.text }];
    if (block.type === "thinking") return [{ id, type: "reasoning", text: block.thinking }];
    if (block.type === "tool_use") {
      const item = { id, type: "mcp_tool_call", server: "CodeBuddy", tool: block.name, arguments: block.input, status: "in_progress" };
      stream?.tools?.set(id, item);
      return [item];
    }
    return [];
  });
  return items;
}

/** CodeBuddy SDK reports completed tools as user/tool_result messages. Re-emit the same item ID
 * so the Rust runtime upserts the loading card instead of adding a second tool card. */
function toolResultItems(message, stream) {
  return (message.message?.content ?? []).flatMap((block) => {
    if (block.type !== "tool_result" || !block.tool_use_id) return [];
    const pending = stream.tools.get(block.tool_use_id);
    if (!pending) return [];
    stream.tools.delete(block.tool_use_id);
    const content = Array.isArray(block.content)
      ? block.content
      : [{ type: "text", text: String(block.content ?? "") }];
    return [{ ...pending, status: block.is_error ? "failed" : "completed", result: { content } }];
  });
}

function completePendingTools(stream) {
  const items = [...stream.tools.values()].map((item) => ({ ...item, status: "completed" }));
  stream.tools.clear();
  return items;
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
  const stream = { messageId: "message", blocks: new Map(), tools: new Map(), finalizedBlocks: [] };
  let sessionId = request.sessionId;
  let checkpoint;
  let activeQuery;
  let liveUsage;
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
      // CodeBuddy 在每次模型请求结束时把真实 usage 放在 assistant.message 上。
      // 逐次累计并立即上报，让工具调用期间标题栏也能更新，而不是等整个 agent turn 结束。
      liveUsage = mergeCodeBuddyUsage(liveUsage, message.message?.usage);
      if (liveUsage) send({ type: "usage", usage: liveUsage });
    }
    else if (message.type === "user") {
      for (const item of toolResultItems(message, stream)) send({ type: "item", item });
    }
    else if (message.type === "error") throw new Error(message.error);
    else if (message.type === "result") {
      if (message.is_error) throw new Error(message.errors?.join("\n") || "CodeBuddy turn failed");
      // Some SDK versions consume tool_result internally without yielding the user message.
      for (const item of completePendingTools(stream)) send({ type: "item", item });
      if (sessionId && checkpoint) send({ type: "checkpoint", sessionId, position: checkpoint });
      // result.usage 是 SDK 汇总终值；缺失时回退到 assistant 事件累计值。
      send({ type: "done", usage: message.usage ?? liveUsage });
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
          _meta: {
            "codex.ai/supportsImages": model.supportsImages ?? false,
            ...(() => {
              const contextWindow = Number(
                model.contextWindow ?? model.context_window ?? model.maxContextTokens ?? model.max_context_tokens,
              );
              return Number.isFinite(contextWindow) && contextWindow >= 2_000
                ? { contextWindow }
                : {};
            })(),
          },
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

export { assistantItems, assistantText, codeBuddyBatchToolPolicy, completePendingTools, mergeCodeBuddyUsage, novaToolsMcpServers, permissionModeFor, promptMessages, resolveCodeBuddyCliPath, streamEventItem, toolResultItems };
