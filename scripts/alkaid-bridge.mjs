import { createInterface } from "node:readline";
import { randomUUID } from "node:crypto";
import { mkdir, readFile, rename, unlink, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { createAlkaidAgent } from "./alkaid-core.mjs";
import { alkaidDataRoot, alkaidModelOptions, defaultAlkaidModel, loadAlkaidConfig, resolveAlkaidModel } from "./alkaid-config.mjs";

const send = (value) => process.stdout.write(`${JSON.stringify(value)}\n`);
const dataRoot = alkaidDataRoot();
const sessionRoot = join(dataRoot, "sessions");
const sessionPath = (sessionId) => {
  if (!/^[A-Za-z0-9_-]+$/.test(sessionId)) throw new Error("非法 Alkaid session id");
  return join(sessionRoot, `${sessionId}.json`);
};

async function mcpServers() {
  const configured = process.env.ALKAID_MCP_SERVERS;
  if (configured) return JSON.parse(configured);
  const text = await readFile(join(dataRoot, "mcp.json"), "utf8").catch(() => "{}");
  return JSON.parse(text);
}

async function loadMessages(sessionId) {
  if (!sessionId) return [];
  return JSON.parse(await readFile(sessionPath(sessionId), "utf8").catch(() => "[]"));
}

async function saveMessages(sessionId, messages) {
  await mkdir(sessionRoot, { recursive: true });
  const path = sessionPath(sessionId);
  const temp = `${path}.${process.pid}.tmp`;
  try {
    await writeFile(temp, JSON.stringify(messages), "utf8");
    await rename(temp, path);
  } catch (error) {
    await unlink(temp).catch(() => {});
    throw error;
  }
}

function promptText(parts = []) {
  return parts.filter((part) => part.type === "text").map((part) => part.text).join("\n\n");
}

function startedToolItem(event) {
  const fileChange = event.toolName === "edit" || event.toolName === "write" || event.toolName === "write_files";
  let type = "mcp_tool_call";
  let command;
  let server = "Alkaid";
  let tool = event.toolName;
  let changes;
  if (event.toolName === "bash") {
    type = "command_execution";
    command = event.args.command;
  } else if (event.toolName.startsWith("mcp__")) {
    [, server, tool] = event.toolName.split("__");
  } else if (fileChange) {
    type = "file_change";
    if (event.toolName === "write_files") {
      changes = (event.args.files ?? []).map((file) => ({ path: file.path, kind: "update" }));
    } else {
      changes = [{ path: event.args.path, kind: "update" }];
    }
  }
  return {
    id: event.toolCallId,
    type,
    status: "in_progress",
    arguments: event.args,
    command,
    server,
    tool,
    changes,
  };
}

async function prompt(request, commands) {
  const task = promptText(request.parts);
  const config = await loadAlkaidConfig({ root: dataRoot });
  const resolved = resolveAlkaidModel(config, request.model);
  const sessionId = request.sessionId || randomUUID();
  const runtime = await createAlkaidAgent({
    cwd: request.cwd,
    model: resolved.model,
    apiKey: resolved.apiKey,
    thinkingLevel: resolved.thinkingLevel ?? request.reasoningEffort,
    mcpServers: await mcpServers(),
    sessionId,
    messages: await loadMessages(request.sessionId),
    readOnly: request.mode === "plan",
  });
  let text = "";
  const assistantId = `assistant-${randomUUID()}`;
  const toolItems = new Map();
  runtime.agent.subscribe((event) => {
    if (event.type === "message_update" && event.assistantMessageEvent.type === "text_delta") {
      text += event.assistantMessageEvent.delta;
      send({ type: "item", item: { id: assistantId, type: "agent_message", text } });
    } else if (event.type === "tool_execution_start") {
      const item = startedToolItem(event);
      toolItems.set(event.toolCallId, item);
      send({ type: "item", item });
    } else if (event.type === "tool_execution_end") {
      const item = toolItems.get(event.toolCallId);
      const output = event.result?.content?.map((part) => part.text ?? "").join("\n") ?? "";
      if (item) send({ type: "item", item: {
        ...item,
        status: event.isError ? "failed" : "completed",
        aggregated_output: output,
        result: event.isError ? undefined : event.result,
        error: event.isError ? { message: output } : undefined,
      } });
    }
  });
  void (async () => {
    for await (const line of commands) {
      if (!line.trim()) continue;
      const command = JSON.parse(line);
      if (command.action === "cancel") {
        runtime.agent.abort();
        return;
      }
    }
  })().catch((error) => send({ type: "error", message: error instanceof Error ? error.message : String(error) }));
  try {
    send({ type: "ready", sessionId });
    await runtime.agent.prompt(task);
    const last = runtime.agent.state.messages.at(-1);
    if (last?.role === "assistant" && last.stopReason === "error") {
      throw new Error(last.errorMessage || "Alkaid provider 请求失败");
    }
    await saveMessages(sessionId, runtime.agent.state.messages);
    send({
      type: "done",
      cancelled: last?.role === "assistant" && last.stopReason === "aborted",
      usage: last?.role === "assistant" ? last.usage : undefined,
    });
  } finally {
    await runtime.close();
  }
}

async function title(request) {
  const config = await loadAlkaidConfig({ root: dataRoot });
  const resolved = resolveAlkaidModel(config, request.model);
  const runtime = await createAlkaidAgent({
    cwd: request.cwd,
    model: resolved.model,
    apiKey: resolved.apiKey,
    thinkingLevel: resolved.thinkingLevel ?? request.reasoningEffort,
  });
  let text = "";
  runtime.agent.subscribe((event) => {
    if (event.type === "message_update" && event.assistantMessageEvent.type === "text_delta") text += event.assistantMessageEvent.delta;
  });
  try {
    await runtime.agent.prompt(request.prompt);
    send({ ok: true, data: text });
  } finally {
    await runtime.close();
  }
}

const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
try {
  const commands = lines[Symbol.asyncIterator]();
  const first = await commands.next();
  if (first.done) throw new Error("Alkaid bridge 缺少请求");
  const request = JSON.parse(first.value);
  if (request.action === "prompt") await prompt(request, commands);
  else if (request.action === "models") {
    const config = await loadAlkaidConfig({ root: dataRoot });
    send({ ok: true, data: { configOptions: [{ id: "model", name: "Model", currentValue: defaultAlkaidModel(config), options: alkaidModelOptions(config) }], modes: null } });
  } else if (request.action === "title") await title(request);
  else throw new Error(`Alkaid bridge 不支持 action: ${request.action}`);
} catch (error) {
  send({ ok: false, error: error instanceof Error ? error.message : String(error) });
  process.exitCode = 1;
} finally {
  lines.close();
}
