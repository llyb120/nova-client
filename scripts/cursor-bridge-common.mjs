import childProcess from "node:child_process";
import { existsSync } from "node:fs";
import { syncBuiltinESMExports } from "node:module";
import { basename, delimiter, join } from "node:path";
import { Cursor } from "@cursor/sdk";

const WINDOWS_SHELL_SHIMS = {
  "bash.exe": "NOVA_SHELL_SHIM_BASH",
  "cmd.exe": "NOVA_SHELL_SHIM_CMD",
  "powershell.exe": "NOVA_SHELL_SHIM_POWERSHELL",
  "pwsh.exe": "NOVA_SHELL_SHIM_PWSH",
};

const WINDOWS_SHELL_REALS = {
  "bash.exe": "NOVA_SHELL_SHIM_BASH_REAL",
  "cmd.exe": "NOVA_SHELL_SHIM_CMD_REAL",
  "powershell.exe": "NOVA_SHELL_SHIM_POWERSHELL_REAL",
  "pwsh.exe": "NOVA_SHELL_SHIM_PWSH_REAL",
};

function envPathValue(env) {
  return Object.entries(env).find(([key]) => key.toLowerCase() === "path")?.[1] ?? "";
}

function findExecutableOnPath(name, env) {
  for (const dir of envPathValue(env).split(delimiter).filter(Boolean)) {
    const candidate = join(dir, name);
    if (existsSync(candidate)) return candidate;
  }
  return null;
}

// 未开启 shim 时，从父进程注入的 *_REAL 或 SystemRoot / PATH / ProgramFiles* 解析真实 shell。
export function resolveWindowsShellFromEnv(name, env = process.env) {
  const real = env[WINDOWS_SHELL_REALS[name]];
  if (real && existsSync(real)) return real;

  if (name === "cmd.exe") {
    for (const root of [env.SystemRoot, env.windir].filter(Boolean)) {
      const candidate = join(root, "System32", "cmd.exe");
      if (existsSync(candidate)) return candidate;
    }
    return findExecutableOnPath("cmd.exe", env);
  }

  if (name === "powershell.exe") {
    for (const root of [env.SystemRoot, env.windir].filter(Boolean)) {
      const candidate = join(root, "System32", "WindowsPowerShell", "v1.0", "powershell.exe");
      if (existsSync(candidate)) return candidate;
    }
    return findExecutableOnPath("powershell.exe", env);
  }

  if (name === "pwsh.exe") {
    const onPath = findExecutableOnPath("pwsh.exe", env);
    if (onPath) return onPath;
    for (const key of ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"]) {
      const root = env[key];
      if (!root) continue;
      const candidate = join(root, "PowerShell", "7", "pwsh.exe");
      if (existsSync(candidate)) return candidate;
    }
    // Cursor SDK 常硬编码 pwsh；本机未安装时回退到 Windows PowerShell。
    return resolveWindowsShellFromEnv("powershell.exe", env);
  }

  if (name === "bash.exe") return findExecutableOnPath("bash.exe", env);
  return null;
}

export function cursorShellProgram(program, env = process.env) {
  if (process.platform !== "win32" || typeof program !== "string") return program;
  const name = basename(program).toLowerCase();
  const shimKey = WINDOWS_SHELL_SHIMS[name];
  if (!shimKey) return program;
  const shim = env[shimKey];
  if (shim) return shim;
  // 未开启 shim（无 SHIM_*）：可用绝对路径保持不变，缺失时从环境变量解析。
  if (existsSync(program)) return program;
  return resolveWindowsShellFromEnv(name, env) || program;
}

function installWindowsShellSpawnGuard() {
  if (process.platform !== "win32") return;
  const spawn = childProcess.spawn;
  const hideWindows = process.env.NOVA_WINDOWS_SHELL_SHIM === "1";
  childProcess.spawn = (program, args, options) => {
    const resolved = cursorShellProgram(program);
    if (Array.isArray(args)) {
      const opts = hideWindows ? { ...options, windowsHide: true } : options;
      return spawn(resolved, args, opts);
    }
    const opts = hideWindows ? { ...args, windowsHide: true } : args;
    return spawn(resolved, opts);
  };
  // Cursor SDK is evaluated before this module body. Keep its named spawn binding synchronized
  // with the guarded built-in implementation without modifying the installed package.
  syncBuiltinESMExports();
}

installWindowsShellSpawnGuard();

/** Cursor SDK NAL stall detector aborts with DOMException AbortError; that can escape the
 *  awaited run chain on a timer and kill the bridge process before turn-level retry runs. */
export function isCursorStallAbortError(error) {
  const seen = new Set();
  let current = error;
  while (current && !seen.has(current)) {
    if (typeof current !== "object") {
      return /This operation was aborted/i.test(String(current));
    }
    seen.add(current);
    if (String(current.name ?? "") === "AbortError") return true;
    // DOMException.ABORT_ERR === 20
    if (current.code === 20 || String(current.code ?? "") === "20") return true;
    const text = String(current.message ?? current.rawMessage ?? "");
    if (/This operation was aborted/i.test(text)) return true;
    current = current.cause;
  }
  return false;
}

/** Transient Cursor/Connect transport failures that turn-level silent retry already knows how to
 *  recover from. Shared so the process guard and prompt loop classify the same errors. */
export function isRetryableCursorError(error) {
  // SDK stall detector / AbortController.abort() → DOMException AbortError.
  if (isCursorStallAbortError(error)) return true;
  const seen = new Set();
  const details = [];
  let current = error;
  while (current && !seen.has(current)) {
    if (typeof current === "object") {
      seen.add(current);
      if (current.isRetryable === true) return true;
      const code = String(current.code ?? "").toLowerCase();
      if (["unavailable", "timeout", "rate_limit", "internal", "aborted", "8", "10", "13", "14"].includes(code)) {
        return true;
      }
      for (const key of ["message", "rawMessage", "details"]) {
        if (current[key] != null) details.push(String(current[key]));
      }
      current = current.cause;
    } else {
      details.push(String(current));
      break;
    }
  }
  return /API key exchange endpoint|fetch failed|ECONNRESET|ECONNREFUSED|ECONNABORTED|ETIMEDOUT|ENETUNREACH|EAI_AGAIN|socket hang up|other side closed|premature close|network connection lost|NGHTTP2_REFUSED_STREAM|\b429\b|\b5\d\d\b/i
    .test(details.join("\n"));
}

function installCursorStallAbortGuard() {
  // Tests import bridge modules; do not override Node's default crash behavior there.
  if (process.env.NOVA_CURSOR_BRIDGE_TEST === "1") return;
  if (globalThis.__novaCursorStallAbortGuardInstalled) return;
  globalThis.__novaCursorStallAbortGuardInstalled = true;

  // Stall aborts and transient SDK side-channel failures (e.g. team-privacy checks during Grep
  // that reject as unhandled ConnectError / NGHTTP2_REFUSED_STREAM) must not kill the bridge.
  // Turn-level silent retry covers awaited prompt failures; writing stderr here would still
  // surface as a Nova bridge failure even if we kept the process alive.
  process.on("unhandledRejection", (reason) => {
    if (isRetryableCursorError(reason)) return;
    process.stderr.write(`Unhandled rejection: ${reason instanceof Error ? reason.stack ?? reason.message : String(reason)}\n`);
    process.exit(1);
  });

  process.on("uncaughtException", (error) => {
    if (isRetryableCursorError(error)) return;
    process.stderr.write(`Uncaught exception: ${error instanceof Error ? error.stack ?? error.message : String(error)}\n`);
    process.exit(1);
  });
}

installCursorStallAbortGuard();

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
    type: "mcp_tool_call",
    server: "Cursor",
    tool,
    arguments: arguments_,
    result: normalizedResult,
    status: status === "error" ? "failed" : status === "running" ? "in_progress" : "completed",
  };
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

let cursorAutoModelId = "auto";

export function modelSelection(selected) {
  if (!selected) return undefined;
  // Cursor SDK local agents require a concrete model selection. The UI sentinel must therefore
  // resolve to the Auto/default model id returned by models.list(), not to an omitted model.
  if (selected === "__cursor_auto__") return { id: cursorAutoModelId };
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
  const contextRules = (() => {
    try {
      const parsed = JSON.parse(process.env.NOVA_CURSOR_MODEL_CONTEXTS || "[]");
      return Array.isArray(parsed)
        ? parsed
          .map((rule) => ({
            prefix: String(rule?.prefix ?? "").trim().toLowerCase(),
            contextWindow: Number.parseInt(rule?.contextWindow ?? "", 10),
          }))
          .filter((rule) => rule.prefix && rule.contextWindow >= 2_000)
          .sort((left, right) => right.prefix.length - left.prefix.length)
        : [];
    } catch {
      return [];
    }
  })();
  const defaultContextWindow = Number.parseInt(process.env.NOVA_CURSOR_CONTEXT_WINDOW || "", 10) || 128_000;
  const contextWindowOf = (model) => {
    const id = String(model?.id ?? "").toLowerCase();
    return contextRules.find((rule) => id.includes(rule.prefix))?.contextWindow ?? defaultContextWindow;
  };
  const withContextWindow = (option, model) => ({
    ...option,
    _meta: { ...(option._meta ?? {}), contextWindow: contextWindowOf(model) },
  });
  const autoModel = models.find((model) => model?.id?.toLowerCase() === "auto")
    ?? models.find((model) => model?.id?.toLowerCase() === "default");
  cursorAutoModelId = autoModel?.id || "auto";
  // 非空哨兵值：与「未选择」区分开，前端显式选中后不会被 resolveAvailableModel 弹回；
  // 发送时由 modelSelection 翻译为 Cursor models.list() 返回的 Auto/default 模型 id。
  const options = [withContextWindow(
    { value: "__cursor_auto__", name: "Auto（自动选具体模型）" },
    autoModel,
  )];
  for (const model of models) {
    if (!model.id || ["auto", "default"].includes(model.id.toLowerCase())) continue;
    if (model.variants?.length) {
      options.push(...model.variants.map((variant) =>
        withContextWindow(encodeModelVariant(model, variant), model)));
    } else {
      options.push(withContextWindow(
        { value: model.id, name: model.displayName, description: model.description },
        model,
      ));
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
