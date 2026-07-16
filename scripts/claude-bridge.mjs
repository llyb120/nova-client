import { createInterface } from "node:readline";
import { existsSync } from "node:fs";
import { delimiter, dirname, isAbsolute, join } from "node:path";
import { query } from "@anthropic-ai/claude-agent-sdk";

const send = (message) => process.stdout.write(`${JSON.stringify(message)}\n`);

function claudePathOverride() {
  const path = process.env.NOVA_CLAUDE_PATH;
  if (process.platform !== "win32" || !path || path.toLowerCase().endsWith(".exe")) return path || undefined;
  const roots = new Set();
  if (isAbsolute(path)) roots.add(dirname(path));
  if (process.env.APPDATA) roots.add(join(process.env.APPDATA, "npm"));
  if (process.env.USERPROFILE) roots.add(join(process.env.USERPROFILE, "AppData", "Roaming", "npm"));
  if (process.env.npm_config_prefix) roots.add(process.env.npm_config_prefix);
  for (const root of (process.env.PATH ?? "").split(delimiter)) {
    if (root && (existsSync(join(root, "claude.cmd")) || existsSync(join(root, "claude.ps1")))) roots.add(root);
  }
  return [...roots]
    .map((root) => join(root, "node_modules", "@anthropic-ai", "claude-code", "bin", "claude.exe"))
    .find(existsSync);
}

function claudeModelOptions(models) {
  return models.flatMap((model) => {
    const efforts = model.supportedEffortLevels ?? [];
    if (!model.supportsEffort || efforts.length === 0) {
      return [{ value: model.value, name: model.displayName, description: model.description }];
    }
    return efforts.map((effort) => ({
      value: `${model.value}:${effort}`,
      name: `${model.displayName} ${effort[0].toUpperCase()}${effort.slice(1)}`,
      description: model.description,
    }));
  });
}

function claudeModelSelection(selected) {
  if (!selected) return {};
  const match = selected.match(/^(.*):(low|medium|high|xhigh|max)$/);
  return match ? { model: match[1], effort: match[2] } : { model: selected };
}

async function modelOptions(request) {
  const activeQuery = query({
    prompt: "",
    options: {
      cwd: request.cwd,
      pathToClaudeCodeExecutable: claudePathOverride(),
    },
  });
  try {
    const models = await activeQuery.supportedModels();
    return {
      configOptions: [{
        id: "model",
        name: "Model",
        currentValue: "",
        options: claudeModelOptions(models),
      }],
      modes: null,
    };
  } finally {
    activeQuery.close();
  }
}

async function main() {
  const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
  const first = await lines[Symbol.asyncIterator]().next();
  if (first.done) throw new Error("Missing request");
  const request = JSON.parse(first.value);
  if (request.action === "models") {
    send({ ok: true, data: await modelOptions(request) });
    lines.close();
    return;
  }
  if (request.action !== "prompt") throw new Error(`Unknown action: ${request.action}`);
  const selection = claudeModelSelection(request.model);
  const controller = new AbortController();
  const pending = new Map();
  void (async () => {
    for await (const line of lines) {
      const command = JSON.parse(line);
      if (command.action === "cancel") controller.abort();
      const resolve = pending.get(command.requestId);
      if (resolve) {
        pending.delete(command.requestId);
        resolve(command.reply === "reject" ? { behavior: "deny", message: "Rejected by user" } : { behavior: "allow" });
      }
    }
  })();
  const prompt = request.parts.filter((part) => part.type === "text").map((part) => part.text).join("\n\n");
  for await (const message of query({
    prompt,
    options: {
      cwd: request.cwd,
      resume: request.sessionId || undefined,
      model: selection.model,
      effort: selection.effort,
      permissionMode: request.mode === "plan" ? "plan" : "default",
      abortController: controller,
      pathToClaudeCodeExecutable: claudePathOverride(),
      canUseTool: (tool, input, options) => new Promise((resolve) => {
        pending.set(options.toolUseID, resolve);
        send({ type: "permission", permission: { id: options.toolUseID, permission: tool, metadata: input } });
      }),
    },
  })) {
    if (message.type === "system" && message.subtype === "init") send({ type: "ready", sessionId: message.session_id });
    if (message.type === "assistant") {
      for (const [index, block] of message.message.content.entries()) {
        const id = block.id ?? `${message.uuid}-${index}`;
        if (block.type === "text") send({ type: "item", item: { id, type: "agent_message", text: block.text } });
        if (block.type === "thinking") send({ type: "item", item: { id, type: "reasoning", text: block.thinking } });
        if (block.type === "tool_use") send({ type: "item", item: { id, type: "mcp_tool_call", server: "Claude", tool: block.name, arguments: block.input, status: "in_progress" } });
      }
    }
    if (message.type === "result") {
      if (message.is_error) throw new Error(message.errors?.join("\n") || "Claude turn failed");
      send({ type: "done", usage: message.usage });
    }
  }
}

if (process.env.NOVA_CLAUDE_BRIDGE_TEST !== "1") main().catch((error) => { send({ ok: false, error: error instanceof Error ? error.message : String(error) }); process.exitCode = 1; });

export { claudeModelOptions, claudeModelSelection };
