import { createInterface } from "node:readline";
import { Agent, Cursor } from "@cursor/sdk";
import { execFile } from "node:child_process";
import { readFile } from "node:fs/promises";
import { extname } from "node:path";
import { promisify } from "node:util";

const send = (message) => process.stdout.write(`${JSON.stringify(message)}\n`);
const execFileAsync = promisify(execFile);

function createMessageState() {
  return { activeTextType: null, textIndex: 0, texts: new Map(), tools: new Map(), deltaTypes: new Set() };
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
      if (block.type === "text" && block.text && !state.deltaTypes.has("assistant")) {
        items.push(appendText(state, message.run_id, "assistant", block.text));
      }
      if (block.type === "tool_use") {
        const item = mapTool(state, block.id, block.name, "running", block.input);
        if (item) items.push(item);
      }
    }
  }
  if (message.type === "thinking" && message.text && !state.deltaTypes.has("thinking")) {
    items.push(appendText(state, message.run_id, "thinking", message.text));
  }
  if (message.type === "tool_call") {
    const item = mapTool(state, message.call_id, message.name, message.status, message.args, message.result);
    if (item) items.push(item);
  }
  return items;
}

function mapDelta(update, state, runId) {
  if (update.type === "text-delta" && update.text) {
    state.deltaTypes.add("assistant");
    return appendText(state, runId, "assistant", update.text);
  }
  if (update.type === "thinking-delta" && update.text) {
    state.deltaTypes.add("thinking");
    return appendText(state, runId, "thinking", update.text);
  }
  if (["tool-call-started", "partial-tool-call", "tool-call-completed"].includes(update.type)) {
    const tool = update.toolCall;
    const failed = tool?.result?.status === "error";
    return mapTool(
      state,
      update.callId,
      tool?.type,
      update.type === "tool-call-completed" ? (failed ? "error" : "completed") : "running",
      tool?.args,
      tool?.result,
    );
  }
  return null;
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
  const separator = selected.indexOf("::");
  if (separator >= 0) {
    const id = selected.slice(0, separator);
    const params = [...new URLSearchParams(selected.slice(separator + 2))]
      .map(([paramId, value]) => ({ id: paramId, value }));
    return { id, ...(params.length ? { params } : {}) };
  }
  const segments = selected.split("-");
  const params = [];
  if (segments.at(-1) === "false") {
    segments.pop();
    params.unshift({ id: "fast", value: "false" });
  }
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
  const params = new URLSearchParams(variant.params.map((param) => [param.id, param.value]));
  const definitions = new Map((model.parameters ?? []).map((param) => [param.id, param]));
  const labels = variant.params.flatMap((param) => {
    if (param.value === "false") return [];
    const definition = definitions.get(param.id);
    if (param.value === "true") return [definition?.displayName ?? param.id];
    const value = definition?.values?.find((item) => item.value === param.value);
    return [value?.displayName ?? param.value];
  });
  return {
    value: `${model.id}::${params}`,
    name: [model.displayName, ...labels].join(" "),
    description: variant.description || model.description,
  };
}

function cursorModelOptions(models) {
  const options = [{ value: "", name: "Auto（Cursor 默认）" }];
  for (const model of models) {
    if (!model.id || ["auto", "default"].includes(model.id.toLowerCase())) continue;
    if (model.variants?.length) {
      options.push(...model.variants.map((variant) => encodeModelVariant(model, variant)));
    } else {
      options.push({ value: model.id, name: model.displayName, description: model.description });
    }
  }
  return options.filter((option, index) =>
    options.findIndex((candidate) => candidate.value === option.value) === index);
}

function parseCliModels(output) {
  return output
    .replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, "")
    .split(/\r?\n/)
    .flatMap((line) => {
      const match = line.trim().match(/^(\S+)\s+-\s+(.+?)(?:\s+\(default\))?$/);
      if (!match || match[1].toLowerCase() === "auto") return [];
      return [{ id: match[1], displayName: match[2] }];
    });
}

async function cliModels() {
  const program = process.env.NOVA_CURSOR_PATH || "cursor-agent";
  const executable = process.platform === "win32" && program.toLowerCase().endsWith(".ps1")
    ? "powershell.exe"
    : program;
  const args = executable === program
    ? ["--list-models"]
    : ["-NoLogo", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", program, "--list-models"];
  const { stdout } = await execFileAsync(executable, args, {
    encoding: "utf8",
    maxBuffer: 1024 * 1024,
    windowsHide: true,
  });
  const models = parseCliModels(stdout);
  if (!models.length) throw new Error("Cursor CLI 未返回模型列表");
  return models;
}

async function modelOptions() {
  let models;
  if (process.env.CURSOR_API_KEY) {
    models = await Cursor.models.list({ apiKey: process.env.CURSOR_API_KEY }).catch(() => undefined);
  }
  models ??= await cliModels();
  return {
    novaCursorModelSchema: 2,
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
  const textParts = parts.filter((part) => part.type === "text").map((part) => part.text);
  const images = [];
  const mediaTypes = { ".jpg": "image/jpeg", ".jpeg": "image/jpeg", ".png": "image/png", ".gif": "image/gif", ".webp": "image/webp" };
  for (const part of parts) {
    if (part.type === "image_data") images.push({ data: part.data, mimeType: part.mime });
    if (part.type === "local_image") {
      const mimeType = mediaTypes[extname(part.path).toLowerCase()];
      if (mimeType) images.push({ data: (await readFile(part.path)).toString("base64"), mimeType });
      else textParts.push(`Attached file: ${part.path}`);
    }
  }
  const text = textParts.join("\n\n");
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
  async function sendPrompt(request, state) {
    const options = {
      mode: request.mode === "plan" ? "plan" : "agent",
      onDelta: ({ update }) => {
        try {
          const item = mapDelta(update, state, activeRun?.id ?? "run");
          if (item) send({ type: "item", item });
        } catch (error) {
          process.stderr.write(`Cursor onDelta failed: ${error instanceof Error ? error.stack ?? error.message : String(error)}\n`);
        }
      },
    };
    try {
      return await agent.send(await promptMessage(request.parts), options);
    } catch (error) {
      if (!String(error).includes("already has active run")) throw error;
      const runs = await Agent.listRuns(agent.agentId, { runtime: "local", cwd: request.cwd });
      for (const run of runs.items) {
        if (run.status === "running") await Agent.cancelRun(run.id, { runtime: "local", cwd: request.cwd });
      }
      const agentId = agent.agentId;
      agent.close();
      agent = await Agent.resume(agentId, {
        apiKey: process.env.CURSOR_API_KEY,
        model: modelSelection(request.model),
        local: { cwd: request.cwd },
      });
      return agent.send(await promptMessage(request.parts), options);
    }
  }
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
      const state = createMessageState();
      activeRun = await sendPrompt(request, state);
      let usage;
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

if (process.env.NOVA_CURSOR_BRIDGE_TEST !== "1") main().catch((error) => {
  process.stderr.write(`${error instanceof Error ? error.stack ?? error.message : String(error)}\n`);
  send({ ok: false, error: error instanceof Error ? error.message : String(error) });
  process.exitCode = 1;
});

export { completePendingTools, createMessageState, cursorModelOptions, mapDelta, mapMessage, modelSelection, parseCliModels, promptMessage };
