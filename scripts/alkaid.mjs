#!/usr/bin/env node
import { createInterface } from "node:readline";
import { createAlkaidAgent } from "./alkaid-core.mjs";
import { loadAlkaidConfig, resolveAlkaidModel } from "./alkaid-config.mjs";

function send(value) {
  process.stdout.write(`${JSON.stringify(value)}\n`);
}

async function run(request) {
  const config = await loadAlkaidConfig();
  const resolved = resolveAlkaidModel(config, request.model);
  const runtime = await createAlkaidAgent({
    ...request,
    model: resolved.model,
    apiKey: resolved.apiKey,
    thinkingLevel: resolved.thinkingLevel ?? request.thinkingLevel ?? request.reasoningEffort,
  });
  let finalText = "";
  runtime.agent.subscribe((event) => {
    if (event.type === "message_update" && event.assistantMessageEvent.type === "text_delta") {
      finalText += event.assistantMessageEvent.delta;
      send({ type: "text_delta", delta: event.assistantMessageEvent.delta });
    } else if (event.type === "tool_execution_start") {
      send({ type: "tool_start", id: event.toolCallId, name: event.toolName, arguments: event.args });
    } else if (event.type === "tool_execution_end") {
      send({ type: "tool_end", id: event.toolCallId, name: event.toolName, isError: event.isError });
    }
  });
  try {
    send({ type: "ready", skills: runtime.skills.map((skill) => skill.name), toolCount: runtime.toolCount });
    await runtime.agent.prompt(request.prompt);
    const last = runtime.agent.state.messages.at(-1);
    if (last?.role === "assistant" && last.stopReason === "error") {
      throw new Error(last.errorMessage || "Alkaid provider 请求失败");
    }
    send({ type: "done", text: finalText });
  } finally {
    await runtime.close();
  }
}

const promptIndex = process.argv.indexOf("--prompt");
if (promptIndex >= 0) {
  run({ prompt: process.argv[promptIndex + 1] ?? "请只回复：Alkaid OK" }).catch((error) => {
    send({ type: "error", error: error instanceof Error ? error.message : String(error) });
    process.exitCode = 1;
  });
} else {
  const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
  const line = await new Promise((resolve) => lines.once("line", resolve));
  lines.close();
  run(JSON.parse(line)).catch((error) => {
    send({ type: "error", error: error instanceof Error ? error.message : String(error) });
    process.exitCode = 1;
  });
}
