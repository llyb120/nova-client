import childProcess from "node:child_process";
import { syncBuiltinESMExports } from "node:module";
import { basename } from "node:path";
import { Cursor } from "@cursor/sdk";

const WINDOWS_SHELL_SHIMS = {
  "bash.exe": "NOVA_SHELL_SHIM_BASH",
  "cmd.exe": "NOVA_SHELL_SHIM_CMD",
  "powershell.exe": "NOVA_SHELL_SHIM_POWERSHELL",
  "pwsh.exe": "NOVA_SHELL_SHIM_PWSH",
};

export function cursorShellProgram(program, env = process.env) {
  if (process.platform !== "win32" || typeof program !== "string") return program;
  const shim = env[WINDOWS_SHELL_SHIMS[basename(program).toLowerCase()]];
  return shim || program;
}

function installWindowsShellSpawnGuard() {
  if (process.platform !== "win32" || process.env.NOVA_WINDOWS_SHELL_SHIM !== "1") return;
  const spawn = childProcess.spawn;
  childProcess.spawn = (program, args, options) => {
    const hiddenOptions = { ...(Array.isArray(args) ? options : args), windowsHide: true };
    if (Array.isArray(args)) return spawn(cursorShellProgram(program), args, hiddenOptions);
    return spawn(cursorShellProgram(program), hiddenOptions);
  };
  // Cursor SDK is evaluated before this module body. Keep its named spawn binding synchronized
  // with the guarded built-in implementation without modifying the installed package.
  syncBuiltinESMExports();
}

installWindowsShellSpawnGuard();

export function createMessageState() {
  return {
    activeTextType: null,
    textIndex: 0,
    texts: new Map(),
    tools: new Map(),
    deltaTypes: new Set(),
    trace: [],
  };
}

export function appendText(state, runId, type, text) {
  if (state.activeTextType !== type) {
    state.activeTextType = type;
    state.textIndex += 1;
  }
  const id = `${runId}-${type}-${state.textIndex}`;
  const combined = `${state.texts.get(id) ?? ""}${text}`;
  state.texts.set(id, combined);
  let trace = state.trace.find((entry) => entry.id === id);
  if (!trace) {
    trace = { id, kind: type, text: "" };
    state.trace.push(trace);
  }
  trace.text = combined;
  return { id, type: type === "assistant" ? "agent_message" : "reasoning", text: combined };
}

export function isEditFilesTool(name) {
  const tool = String(name ?? "");
  return tool === "edit_files" || tool.endsWith("__edit_files") || tool.endsWith("/edit_files");
}

export function isMcpEnvelope(value) {
  return value && typeof value === "object" && !Array.isArray(value)
    && typeof value.toolName === "string";
}

export function mapTool(state, callId, name, status, args, result) {
  const previous = state.tools.get(callId);
  if (previous && previous.status !== "in_progress" && status === "running") return null;
  if (!previous) state.activeTextType = null;
  const envelope = isMcpEnvelope(args) ? args : undefined;
  const genericMcpName = ["mcp", "callMcpTool", "call_mcp_tool"].includes(String(name ?? ""));
  const tool = envelope?.toolName ?? (genericMcpName ? previous?.tool : name) ?? previous?.tool;
  const arguments_ = envelope
    ? envelope.args
    : (args ?? previous?.arguments);
  const resultEnvelope = result ?? previous?.result;
  const normalizedResult = (envelope || genericMcpName) && resultEnvelope?.status === "success"
    && Object.prototype.hasOwnProperty.call(resultEnvelope, "value")
    ? resultEnvelope.value
    : resultEnvelope;
  const item = {
    id: callId,
    type: isEditFilesTool(tool) ? "file_change" : "mcp_tool_call",
    server: "Cursor",
    tool,
    arguments: arguments_,
    result: normalizedResult,
    status: status === "error" ? "failed" : status === "running" ? "in_progress" : "completed",
  };
  if (item.type === "file_change") {
    const files = Array.isArray(arguments_?.files) ? arguments_.files : [];
    item.changes = files
      .map((file) => ({ path: typeof file?.path === "string" ? file.path : "", kind: "update" }))
      .filter((change) => change.path);
  }
  state.tools.set(callId, item);
  let trace = state.trace.find((entry) => entry.id === callId);
  if (!trace) {
    trace = { id: callId, kind: "tool", item };
    state.trace.push(trace);
  } else {
    trace.item = item;
  }
  return item;
}

export function mapMessage(message, state) {
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

export function mapDelta(update, state, runId) {
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

export function completePendingTools(state) {
  const items = [];
  for (const [id, tool] of state.tools) {
    if (tool.status !== "in_progress") continue;
    const completed = { ...tool, id, status: "completed" };
    state.tools.set(id, completed);
    items.push(completed);
  }
  return items;
}

function normalizeTodoStatus(status) {
  if (status === "inProgress") return "in_progress";
  return status ?? "pending";
}

export function cursorTodoPlan(toolCall) {
  if (!toolCall || toolCall.type !== "updateTodos") return null;
  const todos = toolCall.result?.value?.todos ?? toolCall.args?.todos;
  if (!Array.isArray(todos)) return null;
  return todos
    .map((todo) => ({
      content: typeof todo?.content === "string" ? todo.content.trim() : "",
      status: normalizeTodoStatus(todo?.status),
    }))
    .filter((todo) => todo.content);
}

export function modelSelection(selected) {
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

export function encodeModelVariant(model, variant) {
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

export function cursorModelOptions(models) {
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

export async function modelOptions() {
  const apiKey = process.env.CURSOR_API_KEY?.trim();
  if (!apiKey) {
    throw new Error("未配置 Cursor API Key，请在设置 → 模型后端中填写");
  }
  let models;
  try {
    models = await Cursor.models.list({ apiKey });
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    throw new Error(`Cursor SDK 拉取模型失败：${detail}`);
  }
  if (!Array.isArray(models)) {
    throw new Error("Cursor SDK 未返回模型列表");
  }
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
