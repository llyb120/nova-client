import { createInterface } from "node:readline";
import { randomUUID } from "node:crypto";
import { mkdir, readFile, rename, unlink, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { alkaidPromptInput, alkaidUserMessage, createAlkaidAgent, expandAlkaidSkillCommand, mergeAlkaidUsage, runAlkaidPromptWithRetry } from "./alkaid-core.mjs";
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

function startedToolItem(event) {
  const fileChange = event.toolName === "edit" || event.toolName === "write" || event.toolName === "edit_files";
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
    if (event.toolName === "edit_files") {
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
  const input = await alkaidPromptInput(request.parts);
  const config = await loadAlkaidConfig({ root: dataRoot, serverConfig: request.alkaidServerConfig });
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
  let thinking = "";
  let assistantId = `assistant-${randomUUID()}`;
  let thinkingId = `thinking-${randomUUID()}`;
  let reuseAssistantIds = false;
  let cancelled = false;
  let usage;
  const toolItems = new Map();
  runtime.agent.subscribe((event) => {
    if (event.type === "message_end" && event.message.role === "assistant") {
      usage = mergeAlkaidUsage(usage, event.message.usage);
    }
    if (event.type === "message_start" && event.message.role === "assistant") {
      text = "";
      thinking = "";
      if (reuseAssistantIds) {
        reuseAssistantIds = false;
      } else {
        assistantId = `assistant-${randomUUID()}`;
        thinkingId = `thinking-${randomUUID()}`;
      }
    } else if (event.type === "message_update" && event.assistantMessageEvent.type === "text_delta") {
      text += event.assistantMessageEvent.delta;
      send({ type: "item", item: { id: assistantId, type: "agent_message", text } });
    } else if (event.type === "message_update" && event.assistantMessageEvent.type === "thinking_delta") {
      thinking += event.assistantMessageEvent.delta;
      send({ type: "item", item: { id: thinkingId, type: "reasoning", text: thinking } });
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
        cancelled = true;
        runtime.agent.abort();
        return;
      }
      if (command.action === "steer") {
        const message = await alkaidUserMessage(command.parts);
        const textPart = message.content.find((part) => part.type === "text");
        if (textPart) textPart.text = await expandAlkaidSkillCommand(textPart.text, runtime.skills);
        runtime.agent.steer(message);
      }
    }
  })().catch((error) => send({ type: "error", message: error instanceof Error ? error.message : String(error) }));
  try {
    send({ type: "ready", sessionId });
    const expandedText = await expandAlkaidSkillCommand(input.text, runtime.skills);
    const outcome = await runAlkaidPromptWithRetry(runtime.agent, expandedText, input.images, {
      isCancelled: () => cancelled,
      onRetry: () => {
        if (text) send({ type: "item", item: { id: assistantId, type: "agent_message", text: "" } });
        if (thinking) send({ type: "item", item: { id: thinkingId, type: "reasoning", text: "" } });
        text = "";
        thinking = "";
        reuseAssistantIds = true;
      },
    });
    const last = outcome.last;
    if (!outcome.cancelled && last?.role === "assistant" && last.stopReason === "error") {
      throw new Error(last.errorMessage || "Alkaid provider 请求失败");
    }
    await saveMessages(sessionId, runtime.agent.state.messages);
    send({
      type: "done",
      cancelled: outcome.cancelled,
      usage,
    });
  } finally {
    await runtime.close();
  }
}

async function title(request) {
  const config = await loadAlkaidConfig({ root: dataRoot, serverConfig: request.alkaidServerConfig });
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
    const config = await loadAlkaidConfig({ root: dataRoot, serverConfig: request.alkaidServerConfig });
    send({ ok: true, data: { configOptions: [{ id: "model", name: "Model", currentValue: defaultAlkaidModel(config), options: alkaidModelOptions(config) }], modes: null } });
  } else if (request.action === "title") await title(request);
  else throw new Error(`Alkaid bridge 不支持 action: ${request.action}`);
} catch (error) {
  send({ ok: false, error: error instanceof Error ? error.message : String(error) });
  process.exitCode = 1;
} finally {
  lines.close();
}
