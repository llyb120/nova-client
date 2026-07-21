import { createInterface } from "node:readline";
import { randomUUID } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import { createAlkaidAgent } from "./alkaid-core.mjs";

const send = (value) => process.stdout.write(`${JSON.stringify(value)}\n`);
const sessionRoot = join(homedir(), ".nova", "alkaid-sessions");
const sessionPath = (sessionId) => {
  if (!/^[A-Za-z0-9_-]+$/.test(sessionId)) throw new Error("非法 Alkaid session id");
  return join(sessionRoot, `${sessionId}.json`);
};

async function codexConfig() {
  const text = await readFile(join(homedir(), ".codex", "config.toml"), "utf8").catch(() => "");
  const model = text.match(/^model\s*=\s*"([^"]+)"/m)?.[1] ?? "gpt-5.5";
  const provider = text.match(/^model_provider\s*=\s*"([^"]+)"/m)?.[1];
  const escaped = provider?.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const section = escaped ? text.split(new RegExp(`^\\[model_providers\\.${escaped}\\]$`, "m"))[1] ?? "" : "";
  const baseUrl = section.match(/^base_url\s*=\s*"([^"]+)"/m)?.[1] ?? "https://api.openai.com/v1";
  const envKey = section.match(/^env_key\s*=\s*"([^"]+)"/m)?.[1] ?? "OPENAI_API_KEY";
  return { model, baseUrl, apiKey: process.env[envKey], envKey };
}

async function mcpServers() {
  const configured = process.env.ALKAID_MCP_SERVERS;
  if (configured) return JSON.parse(configured);
  const text = await readFile(join(homedir(), ".nova", "mcp.json"), "utf8").catch(() => "{}");
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
  const config = await codexConfig();
  if (!config.apiKey) throw new Error(`本机 Codex 凭据变量 ${config.envKey} 未注入 Nova 进程`);
  const sessionId = request.sessionId || randomUUID();
  const runtime = await createAlkaidAgent({
    cwd: request.cwd,
    model: request.model || config.model,
    baseUrl: config.baseUrl,
    apiKey: config.apiKey,
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
      const type = event.toolName.startsWith("mcp__") ? "mcp_tool_call" : event.toolName === "write_files" ? "file_change" : "command_execution";
      const [, server, tool] = event.toolName.split("__");
      const item = {
        id: event.toolCallId,
        type,
        status: "in_progress",
        arguments: event.args,
        command: event.toolName,
        server,
        tool,
        changes: event.toolName === "write_files"
          ? (event.args.files ?? []).map((file) => ({ path: file.path, kind: "update" }))
          : undefined,
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
    await saveMessages(sessionId, runtime.agent.state.messages);
    const last = runtime.agent.state.messages.at(-1);
    send({ type: "done", usage: last?.role === "assistant" ? last.usage : undefined });
  } finally {
    await runtime.close();
  }
}

async function title(request) {
  const config = await codexConfig();
  const runtime = await createAlkaidAgent({ cwd: request.cwd, model: request.model || config.model, baseUrl: config.baseUrl, apiKey: config.apiKey });
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
    const config = await codexConfig();
    send({ ok: true, data: { configOptions: [{ id: "model", name: "Model", currentValue: config.model, options: [{ value: config.model, name: config.model }] }], modes: null } });
  } else if (request.action === "title") await title(request);
  else throw new Error(`Alkaid bridge 不支持 action: ${request.action}`);
} catch (error) {
  send({ ok: false, error: error instanceof Error ? error.message : String(error) });
  process.exitCode = 1;
} finally {
  lines.close();
}
