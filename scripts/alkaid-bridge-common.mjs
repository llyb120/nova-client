import { createInterface } from "node:readline";
import { mkdir, readFile, rename, unlink, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { streamSimple } from "@earendil-works/pi-ai/compat";
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
  const args = event.args && typeof event.args === "object" ? event.args : {};
  let type = "mcp_tool_call";
  let command;
  let server = "Vega";
  let tool = event.toolName;
  let changes;
  if (event.toolName === "bash") {
    type = "command_execution";
    command = args.command;
  } else if (event.toolName.startsWith("mcp__")) {
    [, server, tool] = event.toolName.split("__");
  } else if (fileChange) {
    type = "file_change";
    const files = event.toolName === "edit_files"
      ? (Array.isArray(args.files) ? args.files : [])
      : [args];
    changes = files
      .filter((file) => file && typeof file.path === "string")
      .map((file) => ({ path: file.path, kind: "update" }));
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

/// 输入框补全：不建 agent，只对模型 API 直接发一次 completion。
async function complete(request) {
  const config = await loadAlkaidConfig({ root: dataRoot, serverConfig: request.alkaidServerConfig });
  // 空 model = 未配置补全模型，回退到 Vega 默认模型
  const resolved = resolveAlkaidModel(config, request.model || undefined);
  const stream = streamSimple(
    resolved.model,
    { messages: [{ role: "user", content: request.prompt, timestamp: Date.now() }] },
    // 直调没有 agent 的 getApiKey 兜底，必须显式带上解析出的 key，否则请求无鉴权
    { reasoning: "minimal", apiKey: resolved.apiKey },
  );
  let text = "";
  for await (const event of stream) {
    if (event.type === "message_update" && event.assistantMessageEvent.type === "text_delta") {
      text += event.assistantMessageEvent.delta;
    }
  }
  const result = await stream.result();
  if (result.stopReason === "error") throw new Error(result.errorMessage ?? "Vega 补全调用失败");
  send({ ok: true, data: text.trim() });
}

async function title(request) {
  const config = await loadAlkaidConfig({ root: dataRoot, serverConfig: request.alkaidServerConfig });
  // 空 model = 未配置轻量模型，回退到 Vega 默认模型
  const resolved = resolveAlkaidModel(config, request.model || undefined);
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
    else if (request.action === "complete") await complete(request);
    else throw new Error(`Vega bridge 不支持 action: ${request.action}`);
  } catch (error) {
    send({ ok: false, error: error instanceof Error ? error.message : String(error) });
    process.exitCode = 1;
  } finally {
    lines.close();
  }
}
