import { createInterface } from "node:readline";
import { randomUUID } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
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
  await writeFile(sessionPath(sessionId), JSON.stringify(messages), "utf8");
}

function promptText(parts = []) {
  return parts.filter((part) => part.type === "text").map((part) => part.text).join("\n\n");
}

async function prompt(request) {
  const task = promptText(request.parts);
  const config = await loadAlkaidConfig({ root: dataRoot });
  const resolved = resolveAlkaidModel(config, request.model);
  const sessionId = request.sessionId || randomUUID();
  const runtime = await createAlkaidAgent({
    cwd: request.cwd,
    model: resolved.model,
    apiKey: resolved.apiKey,
    thinkingLevel: request.reasoningEffort || "high",
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
      const fileChange = event.toolName === "edit" || event.toolName === "write" || event.toolName === "write_files";
      let type = "command_execution";
      if (event.toolName.startsWith("mcp__")) type = "mcp_tool_call";
      else if (fileChange) type = "file_change";
      const [, server, tool] = event.toolName.split("__");
      let changes;
      if (event.toolName === "write_files") {
        changes = (event.args.files ?? []).map((file) => ({ path: file.path, kind: "update" }));
      } else if (fileChange) {
        changes = [{ path: event.args.path, kind: "update" }];
      }
      const item = {
        id: event.toolCallId,
        type,
        status: "in_progress",
        arguments: event.args,
        command: event.toolName,
        server,
        tool,
        changes,
      };
      toolItems.set(event.toolCallId, item);
      send({ type: "item", item });
    } else if (event.type === "tool_execution_end") {
      const item = toolItems.get(event.toolCallId);
      if (item) send({ type: "item", item: {
        ...item,
        status: event.isError ? "failed" : "completed",
        aggregated_output: event.result?.content?.map((part) => part.text ?? "").join("\n") ?? "",
      } });
    }
  });
  try {
    send({ type: "ready", sessionId });
    await runtime.agent.prompt(task);
    const last = runtime.agent.state.messages.at(-1);
    if (last?.role === "assistant" && last.stopReason === "error") {
      throw new Error(last.errorMessage || "Alkaid provider 请求失败");
    }
    await saveMessages(sessionId, runtime.agent.state.messages);
    send({ type: "done", usage: last?.role === "assistant" ? last.usage : undefined });
  } finally {
    await runtime.close();
  }
}

async function title(request) {
  const config = await loadAlkaidConfig({ root: dataRoot });
  const resolved = resolveAlkaidModel(config, request.model);
  const runtime = await createAlkaidAgent({ cwd: request.cwd, model: resolved.model, apiKey: resolved.apiKey });
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
  const request = JSON.parse(await new Promise((resolve) => lines.once("line", resolve)));
  if (request.action === "prompt") await prompt(request);
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
