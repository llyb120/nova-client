#!/usr/bin/env node
import { createInterface } from "node:readline";
import { readFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import { createAlkaidAgent } from "./alkaid-core.mjs";

async function localCodexConfig() {
  const text = await readFile(join(homedir(), ".codex", "config.toml"), "utf8").catch(() => "");
  const model = text.match(/^model\s*=\s*"([^"]+)"/m)?.[1] ?? "gpt-5.5";
  const provider = text.match(/^model_provider\s*=\s*"([^"]+)"/m)?.[1];
  const section = provider ? text.split(new RegExp(`^\\[model_providers\\.${provider.replace(/[.*+?^${}()|[\\]\\]/g, "\\$&")}\\]$`, "m"))[1] ?? "" : "";
  const baseUrl = section.match(/^base_url\s*=\s*"([^"]+)"/m)?.[1] ?? process.env.ALKAID_BASE_URL ?? "http://127.0.0.1:8317/v1";
  const envKey = section.match(/^env_key\s*=\s*"([^"]+)"/m)?.[1] ?? "OPENAI_API_KEY";
  return { model, baseUrl, envKey, apiKey: process.env[envKey] };
}

function send(value) {
  process.stdout.write(`${JSON.stringify(value)}\n`);
}

async function run(request) {
  const local = await localCodexConfig();
  if (!local.apiKey) throw new Error(`本机 Codex 凭据变量 ${local.envKey} 未注入当前进程`);
  const runtime = await createAlkaidAgent({ ...local, ...request });
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
