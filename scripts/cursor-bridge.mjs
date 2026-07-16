import { createInterface } from "node:readline";
import { Agent, Cursor } from "@cursor/sdk";
import { readFile } from "node:fs/promises";
import { extname } from "node:path";

const send = (message) => process.stdout.write(`${JSON.stringify(message)}\n`);

function createMessageState() {
  return { activeTextType: null, textIndex: 0, texts: new Map(), tools: new Map() };
}

function appendText(state, runId, type, text) {
  if (state.activeTextType !== type) {
    state.activeTextType = type;
    state.textIndex += 1;
  }
  const id = `${runId}-${type}-${state.textIndex}`;
  const combined = `${state.texts.get(id) ?? ""}${text}`;
  state.texts.set(id, combined);
  return { id, type: type === "assistant" ? "agent_message" : "reasoning", text: combined };
}

function mapTool(state, callId, name, status, args, result) {
  const previous = state.tools.get(callId);
  if (previous && previous.status !== "in_progress" && status === "running") return null;
  if (!previous) state.activeTextType = null;
  const item = {
    id: callId,
    type: "mcp_tool_call",
    server: "Cursor",
    tool: name ?? previous?.tool,
    arguments: args ?? previous?.arguments,
    result: result ?? previous?.result,
    status: status === "error" ? "failed" : status === "running" ? "in_progress" : "completed",
  };
  state.tools.set(callId, item);
  return item;
}

function mapMessage(message, state) {
  const items = [];
  if (message.type === "assistant") {
    for (const block of message.message.content) {
      if (block.type === "text" && block.text) {
        items.push(appendText(state, message.run_id, "assistant", block.text));
      }
      if (block.type === "tool_use") {
        const item = mapTool(state, block.id, block.name, "running", block.input);
        if (item) items.push(item);
      }
    }
  }
  if (message.type === "thinking" && message.text) {
    items.push(appendText(state, message.run_id, "thinking", message.text));
  }
  if (message.type === "tool_call") {
    const item = mapTool(state, message.call_id, message.name, message.status, message.args, message.result);
    if (item) items.push(item);
  }
  return items;
}

function completePendingTools(state) {
  const items = [];
  for (const [id, tool] of state.tools) {
    if (tool.status !== "in_progress") continue;
    const completed = { ...tool, id, status: "completed" };
    state.tools.set(id, completed);
    items.push(completed);
  }
  return items;
}

function modelSelection(selected) {
  if (!selected) return undefined;
  const segments = selected.split("-");
  const params = [];
  if (segments.at(-1) === "fast") {
    segments.pop();
    params.unshift({ id: "fast", value: "true" });
  }
  const efforts = new Set(["none", "low", "medium", "high", "xhigh", "max"]);
  if (efforts.has(segments.at(-1))) params.unshift({ id: "effort", value: segments.pop() });
  if (segments[0] === "cursor" && segments[1] === "grok") segments.shift();
  return { id: segments.join("-"), ...(params.length ? { params } : {}) };
}

function encodeModelVariant(model, variant) {
  const suffix = variant.params
    .map((param) => param.id === "fast" && param.value === "true" ? "fast" : param.value)
    .filter(Boolean)
    .join("-");
  return {
    value: suffix ? `${model.id}-${suffix}` : model.id,
    name: variant.displayName || `${model.displayName} ${suffix}`,
    description: variant.description || model.description,
  };
}

function cursorModelOptions(models) {
  const options = [{ value: "", name: "Auto（Cursor 默认）" }];
  for (const model of models) {
    if (!model.id || model.id.toLowerCase() === "auto") continue;
    if (model.variants?.length) {
      options.push(...model.variants.map((variant) => encodeModelVariant(model, variant)));
    } else {
      options.push({ value: model.id, name: model.displayName, description: model.description });
    }
  }
  return options.filter((option, index) =>
    options.findIndex((candidate) => candidate.value === option.value) === index);
}

async function modelOptions() {
  const models = process.env.CURSOR_API_KEY
    ? await Cursor.models.list({ apiKey: process.env.CURSOR_API_KEY }).catch(() => [])
    : [];
  return {
    configOptions: [{
      id: "model",
      name: "Model",
      currentValue: "",
      options: cursorModelOptions(models),
    }],
    modes: null,
  };
}

async function promptMessage(parts) {
  const text = parts.filter((part) => part.type === "text").map((part) => part.text).join("\n\n");
  const images = [];
  const mediaTypes = { ".jpg": "image/jpeg", ".jpeg": "image/jpeg", ".png": "image/png", ".gif": "image/gif", ".webp": "image/webp" };
  for (const part of parts) {
    if (part.type === "image_data") images.push({ data: part.data, mimeType: part.mime });
    if (part.type === "local_image") {
      const mimeType = mediaTypes[extname(part.path).toLowerCase()];
      if (!mimeType) throw new Error(`Unsupported image type: ${part.path}`);
      images.push({ data: (await readFile(part.path)).toString("base64"), mimeType });
    }
  }
  return images.length ? { text, images } : text;
}

async function main() {
  const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
  const requests = [];
  let wake;
  let activeRun;
  let closed = false;
  lines.on("line", (line) => {
    const request = JSON.parse(line);
    if (request.action === "cancel") {
      void activeRun?.cancel();
      return;
    }
    requests.push(request);
    wake?.();
    wake = undefined;
  });
  lines.on("close", () => {
    closed = true;
    wake?.();
  });
  let agent;
  while (!closed || requests.length) {
    if (!requests.length) await new Promise((resolve) => { wake = resolve; });
    const request = requests.shift();
    if (!request) continue;
    try {
      if (request.action === "models") {
        send({ ok: true, data: await modelOptions() });
        continue;
      }
      if (request.action !== "prompt") throw new Error(`Unknown action: ${request.action}`);
      if (!agent) {
        const options = { apiKey: process.env.CURSOR_API_KEY, model: modelSelection(request.model), local: { cwd: request.cwd } };
        agent = request.sessionId ? await Agent.resume(request.sessionId, options) : await Agent.create(options);
      }
      send({ type: "ready", sessionId: agent.agentId });
      activeRun = await agent.send(await promptMessage(request.parts), { mode: request.mode === "plan" ? "plan" : "agent" });
      let usage;
      const state = createMessageState();
      for await (const message of activeRun.stream()) {
        for (const item of mapMessage(message, state)) send({ type: "item", item });
        if (message.type === "usage") usage = message.usage;
      }
      const result = await activeRun.wait();
      for (const item of completePendingTools(state)) send({ type: "item", item });
      if (result.status === "error") throw new Error(result.error?.message || "Cursor turn failed");
      send({ type: "done", usage: usage ?? result.usage });
    } catch (error) {
      send({ ok: false, error: error instanceof Error ? error.message : String(error) });
    } finally {
      activeRun = undefined;
    }
  }
  agent?.close();
}

if (process.env.NOVA_CURSOR_BRIDGE_TEST !== "1") main().catch((error) => { send({ ok: false, error: error instanceof Error ? error.message : String(error) }); process.exitCode = 1; });

export { completePendingTools, createMessageState, cursorModelOptions, mapMessage, modelSelection, promptMessage };
