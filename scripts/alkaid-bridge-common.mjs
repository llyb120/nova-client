import { createInterface } from "node:readline";
import { mkdir, readFile, rename, unlink, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { createAlkaidAgent } from "./alkaid-core.mjs";
import { alkaidDataRoot, alkaidModelOptions, defaultAlkaidModel, loadAlkaidConfig, resolveAlkaidModel } from "./alkaid-config.mjs";

export function send(value) {
  process.stdout.write(`${JSON.stringify(value)}\n`);
}

export const dataRoot = alkaidDataRoot();
const sessionRoot = join(dataRoot, "sessions");

export function sessionPath(sessionId) {
  if (!/^[A-Za-z0-9_-]+$/.test(sessionId)) throw new Error("非法 Vega session id");
  return join(sessionRoot, `${sessionId}.json`);
}

export async function mcpServers() {
  const configured = process.env.ALKAID_MCP_SERVERS;
  if (configured) return JSON.parse(configured);
  const text = await readFile(join(dataRoot, "mcp.json"), "utf8").catch(() => "{}");
  return JSON.parse(text);
}

export async function loadMessages(sessionId) {
  if (!sessionId) return [];
  return JSON.parse(await readFile(sessionPath(sessionId), "utf8").catch(() => "[]"));
}

export async function saveJson(path, value) {
  await mkdir(sessionRoot, { recursive: true });
  const temp = `${path}.${process.pid}.tmp`;
  try {
    await writeFile(temp, JSON.stringify(value), "utf8");
    await rename(temp, path);
  } catch (error) {
    await unlink(temp).catch(() => {});
    throw error;
  }
}

export function saveMessages(sessionId, messages) {
  return saveJson(sessionPath(sessionId), messages);
}

export function startedToolItem(event) {
  const fileChange = event.toolName === "edit" || event.toolName === "write" || event.toolName === "edit_files";
  let type = "mcp_tool_call";
  let command;
  let server = "Vega";
  let tool = event.toolName;
  let changes;
  if (event.toolName === "bash") {
    type = "command_execution";
    command = event.args.command;
  } else if (event.toolName.startsWith("mcp__")) {
    [, server, tool] = event.toolName.split("__");
  } else if (fileChange) {
    type = "file_change";
    changes = event.toolName === "edit_files"
      ? (event.args.files ?? []).map((file) => ({ path: file.path, kind: "update" }))
      : [{ path: event.args.path, kind: "update" }];
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

export async function runAlkaidBridge(handlePrompt) {
  const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
  try {
    const commands = lines[Symbol.asyncIterator]();
    const first = await commands.next();
    if (first.done) throw new Error("Vega bridge 缺少请求");
    const request = JSON.parse(first.value);
    if (request.action === "prompt") await handlePrompt(request, commands);
    else if (request.action === "models") {
      const config = await loadAlkaidConfig({ root: dataRoot, serverConfig: request.alkaidServerConfig });
      send({ ok: true, data: { configOptions: [{ id: "model", name: "Model", currentValue: defaultAlkaidModel(config), options: alkaidModelOptions(config) }], modes: null } });
    } else if (request.action === "title") await title(request);
    else throw new Error(`Vega bridge 不支持 action: ${request.action}`);
  } catch (error) {
    send({ ok: false, error: error instanceof Error ? error.message : String(error) });
    process.exitCode = 1;
  } finally {
    lines.close();
  }
}
