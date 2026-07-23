import { createInterface } from "node:readline";
import { randomUUID } from "node:crypto";
import { mkdir, readFile, rename, unlink, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { alkaidPromptInput, alkaidUserMessage, createAlkaidAgent, expandAlkaidSkillCommand, mergeAlkaidUsage, runAlkaidPromptWithRetry } from "./alkaid-core.mjs";
import { appendSlimTurn, compactSlimMemory, contextTokensFromMessages, createSlimMemory, formatSlimMemory, memoryWithoutCurrent, seedSlimMemoryFromMessages, setLatestConclusion, shouldUseFullContext } from "./alkaid-slim-memory.mjs";
import { alkaidDataRoot, alkaidModelOptions, defaultAlkaidModel, loadAlkaidConfig, resolveAlkaidModel } from "./alkaid-config.mjs";

const send = (value) => process.stdout.write(`${JSON.stringify(value)}\n`);
const dataRoot = alkaidDataRoot();
const sessionRoot = join(dataRoot, "sessions");
const sessionPath = (sessionId) => {
  if (!/^[A-Za-z0-9_-]+$/.test(sessionId)) throw new Error("非法 Vega session id");
  return join(sessionRoot, `${sessionId}.json`);
};

async function mcpServers() {
  const configured = process.env.ALKAID_MCP_SERVERS;
  if (configured) return JSON.parse(configured);
  const text = await readFile(join(dataRoot, "mcp.json"), "utf8").catch(() => "{}");
  return JSON.parse(text);
}

async function loadMessages(sessionId) {
  if (!sessionId) return [];
  return JSON.parse(await readFile(sessionPath(sessionId), "utf8").catch(() => "[]"));
}

async function saveJson(path, value) {
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

async function saveMessages(sessionId, messages) {
  await saveJson(sessionPath(sessionId), messages);
}

const slimMemoryPath = (sessionId) => sessionPath(sessionId).replace(/\.json$/, ".slim.json");

async function loadSlimMemory(sessionId) {
  if (!sessionId) return createSlimMemory();
  try {
    const parsed = JSON.parse(await readFile(slimMemoryPath(sessionId), "utf8"));
    return Array.isArray(parsed?.turns)
      ? {
          summary: String(parsed.summary ?? ""),
          turns: parsed.turns,
          pendingMessages: Array.isArray(parsed.pendingMessages) ? parsed.pendingMessages : [],
          fullMessages: Array.isArray(parsed.fullMessages) ? parsed.fullMessages : [],
          contextTokens: Number(parsed.contextTokens) || 0,
          contextStage: parsed.contextStage === "slim" ? "slim" : "full",
        }
      : createSlimMemory();
  } catch {
    return createSlimMemory();
  }
}

async function saveSlimMemory(sessionId, memory) {
  await saveJson(slimMemoryPath(sessionId), { version: 1, ...memory });
}

function messageWithSlimMemory(text, memory) {
  const context = formatSlimMemory(memoryWithoutCurrent(memory, {
    pendingMessages: memory.pendingMessages?.length > 0,
  }));
  if (!context) return text;
  return [
    "请仅使用下面的精简记忆延续会话。完整工具轨迹和原始对话已被有意省略。",
    "不要要求用户重复之前的要求；结合记忆和当前请求继续工作。",
    "",
    context,
    "",
    "当前请求：",
    text,
  ].join("\n");
}

function startedToolItem(event) {
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
    if (event.toolName === "edit_files") {
      changes = (event.args.files ?? []).map((file) => ({ path: file.path, kind: "update" }));
    } else {
      changes = [{ path: event.args.path, kind: "update" }];
    }
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

async function prompt(request, commands) {
  const input = await alkaidPromptInput(request.parts);
  const config = await loadAlkaidConfig({ root: dataRoot, serverConfig: request.alkaidServerConfig });
  const resolved = resolveAlkaidModel(config, request.model);
  const sessionId = request.sessionId || randomUUID();
  const slimContext = request.vegaSlimContext === true;
  let memory = createSlimMemory();
  let useFullContext = false;
  let enteredSlimStage = false;
  let maxContextTokens = Number.POSITIVE_INFINITY;
  let maxContextChars = Number.POSITIVE_INFINITY;
  if (slimContext) {
    memory = await loadSlimMemory(sessionId);
    if (!memory.summary && !memory.turns.length && request.sessionId) {
      seedSlimMemoryFromMessages(memory, await loadMessages(request.sessionId));
    }
    maxContextTokens = Math.max(2_000, Math.floor(Number(resolved.model.contextWindow ?? 128_000) * 0.8));
    maxContextChars = Math.max(8_000, Math.floor(Number(resolved.model.contextWindow ?? 128_000) * 0.8));
    useFullContext = shouldUseFullContext(memory, maxContextTokens, maxContextChars);
    if (!useFullContext && memory.contextStage === "full") {
      // Stage one only drops native thinking/tool trajectories. Token usage from that native
      // request must not immediately trigger stage-two summarization.
      memory.contextStage = "slim";
      memory.contextTokens = 0;
      memory.fullMessages = [];
      enteredSlimStage = true;
    }
    appendSlimTurn(memory, input.text);
    const compacted = !enteredSlimStage && await compactSlimMemory(memory, async (earlier) => {
      const summaryRuntime = await createAlkaidAgent({
        cwd: request.cwd,
        model: resolved.model,
        apiKey: resolved.apiKey,
        thinkingLevel: resolved.thinkingLevel ?? request.reasoningEffort,
      });
      let summary = "";
      summaryRuntime.agent.subscribe((event) => {
        if (event.type === "message_update" && event.assistantMessageEvent.type === "text_delta") {
          summary += event.assistantMessageEvent.delta;
        }
      });
      try {
        await summaryRuntime.agent.prompt([
          "请把下面较早的会话记忆压缩成供另一个编码 Agent 使用的摘要。",
          "保留用户意图、决策、改动文件、关键标识、约束和未完成事项；不要照抄对话或添加评论。",
          "",
          earlier,
        ].join("\n"));
        return summary;
      } finally {
        await summaryRuntime.close();
      }
    }, {
      // Stage two is capacity-based: only summarize after prompt/conclusion memory itself reaches
      // the limit. The turn threshold is exclusively a stage-one transition trigger.
      maxTurns: Number.POSITIVE_INFINITY,
      currentTokens: memory.contextStage === "slim" ? memory.contextTokens : 0,
      maxTokens: maxContextTokens,
      // Keep the character estimate only when the provider reports no token usage.
      maxChars: memory.contextTokens > 0 ? Number.POSITIVE_INFINITY : maxContextChars,
    });
    if (compacted) memory.contextTokens = 0;
  }
  let nativeMessages;
  if (!slimContext) nativeMessages = await loadMessages(request.sessionId);
  else if (memory.pendingMessages?.length) nativeMessages = memory.pendingMessages;
  else nativeMessages = useFullContext ? memory.fullMessages : [];
  const runtime = await createAlkaidAgent({
    cwd: request.cwd,
    model: resolved.model,
    apiKey: resolved.apiKey,
    thinkingLevel: resolved.thinkingLevel ?? request.reasoningEffort,
    mcpServers: await mcpServers(),
    sessionId,
    // Early turns and interrupted work retain the native message/tool trajectory. Once either
    // threshold is reached, compact memory replaces completed trajectories as usual.
    messages: nativeMessages,
    readOnly: request.mode === "plan",
  });
  let text = "";
  let thinking = "";
  let assistantId = `assistant-${randomUUID()}`;
  let thinkingId = `thinking-${randomUUID()}`;
  let reuseAssistantIds = false;
  let cancelled = false;
  let usage;
  const toolItems = new Map();
  runtime.agent.subscribe((event) => {
    if (event.type === "message_end" && event.message.role === "assistant") {
      usage = mergeAlkaidUsage(usage, event.message.usage);
    }
    if (event.type === "message_start" && event.message.role === "assistant") {
      text = "";
      thinking = "";
      if (reuseAssistantIds) {
        reuseAssistantIds = false;
      } else {
        assistantId = `assistant-${randomUUID()}`;
        thinkingId = `thinking-${randomUUID()}`;
      }
    } else if (event.type === "message_update" && event.assistantMessageEvent.type === "text_delta") {
      text += event.assistantMessageEvent.delta;
      send({ type: "item", item: { id: assistantId, type: "agent_message", text } });
    } else if (event.type === "message_update" && event.assistantMessageEvent.type === "thinking_delta") {
      thinking += event.assistantMessageEvent.delta;
      send({ type: "item", item: { id: thinkingId, type: "reasoning", text: thinking } });
    } else if (event.type === "tool_execution_start") {
      const item = startedToolItem(event);
      toolItems.set(event.toolCallId, item);
      send({ type: "item", item });
    } else if (event.type === "tool_execution_end") {
      const item = toolItems.get(event.toolCallId);
      const output = event.result?.content?.map((part) => part.text ?? "").join("\n") ?? "";
      if (item) send({ type: "item", item: {
        ...item,
        status: event.isError ? "failed" : "completed",
        aggregated_output: output,
        result: event.isError ? undefined : event.result,
        error: event.isError ? { message: output } : undefined,
      } });
    }
  });
  void (async () => {
    for await (const line of commands) {
      if (!line.trim()) continue;
      const command = JSON.parse(line);
      if (command.action === "cancel") {
        cancelled = true;
        runtime.agent.abort();
        return;
      }
      if (command.action === "steer") {
        const message = await alkaidUserMessage(command.parts);
        const textPart = message.content.find((part) => part.type === "text");
        if (textPart) {
          if (slimContext) appendSlimTurn(memory, textPart.text);
          textPart.text = await expandAlkaidSkillCommand(textPart.text, runtime.skills);
        }
        runtime.agent.steer(message);
      }
    }
  })().catch((error) => send({ type: "error", message: error instanceof Error ? error.message : String(error) }));
  try {
    send({ type: "ready", sessionId });
    const expandedText = await expandAlkaidSkillCommand(input.text, runtime.skills);
    const promptText = slimContext && !useFullContext ? messageWithSlimMemory(expandedText, memory) : expandedText;
    const outcome = await runAlkaidPromptWithRetry(runtime.agent, promptText, input.images, {
      isCancelled: () => cancelled,
      onRetry: () => {
        if (text) send({ type: "item", item: { id: assistantId, type: "agent_message", text: "" } });
        if (thinking) send({ type: "item", item: { id: thinkingId, type: "reasoning", text: "" } });
        text = "";
        thinking = "";
        reuseAssistantIds = true;
      },
    });
    const last = outcome.last;
    if (!outcome.cancelled && last?.role === "assistant" && last.stopReason === "error") {
      throw new Error(last.errorMessage || "Vega provider 请求失败");
    }
    if (slimContext) {
      if (!outcome.cancelled && last?.role === "assistant" && last.stopReason !== "error") {
        setLatestConclusion(memory, last.content);
        memory.pendingMessages = [];
        const measuredTokens = contextTokensFromMessages(runtime.agent.state.messages);
        if (memory.contextStage === "full") {
          memory.contextTokens = measuredTokens;
          const belowCapacity = measuredTokens > 0
            ? measuredTokens < maxContextTokens
            : JSON.stringify(runtime.agent.state.messages).length < maxContextChars;
          if (memory.turns.length < 10 && belowCapacity) {
            memory.fullMessages = structuredClone(runtime.agent.state.messages);
          } else {
            // Enter stage two without summarizing yet. Its own usage is measured on the next turn.
            memory.contextStage = "slim";
            memory.contextTokens = 0;
            memory.fullMessages = [];
          }
        } else {
          memory.contextTokens = measuredTokens;
          memory.fullMessages = [];
        }
      } else if (outcome.cancelled) {
        memory.pendingMessages = structuredClone(runtime.agent.state.messages);
      }
      await saveSlimMemory(sessionId, memory);
    } else {
      await saveMessages(sessionId, runtime.agent.state.messages);
    }
    send({
      type: "done",
      cancelled: outcome.cancelled,
      usage,
    });
  } finally {
    if (slimContext) await saveSlimMemory(sessionId, memory).catch(() => {});
    await runtime.close();
  }
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

const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
try {
  const commands = lines[Symbol.asyncIterator]();
  const first = await commands.next();
  if (first.done) throw new Error("Vega bridge 缺少请求");
  const request = JSON.parse(first.value);
  if (request.action === "prompt") await prompt(request, commands);
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
