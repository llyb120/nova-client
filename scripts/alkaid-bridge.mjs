import { createInterface } from "node:readline";
import { randomUUID } from "node:crypto";
import { mkdir, readFile, rename, unlink, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { ALKAID_PROVIDER_IDLE_TIMEOUT_ENABLED, ALKAID_PROVIDER_IDLE_TIMEOUT_MS, alkaidPromptInput, alkaidUserMessage, createAlkaidAgent, createAlkaidIdleTimeout, expandAlkaidSkillCommand, mergeAlkaidUsage, messagesWithPendingAlkaidPrompt, restoreAlkaidSteeringForRetry, runAlkaidPromptWithRetry } from "./alkaid-core.mjs";
import { alkaidDiagnosticEndpoint, createAlkaidDiagnosticLog } from "./alkaid-diagnostics.mjs";
import { appendSlimTurn, compactSlimMemory, contextTokensFromMessages, createSlimMemory, estimateContextTokens, formatSlimMemory, memoryWithoutCurrent, seedSlimMemoryFromMessages, setLatestConclusion, shouldUseFullContext, stripCompletedOpenAIReasoning } from "./alkaid-slim-memory.mjs";
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
  let maxContextTokens = Number.POSITIVE_INFINITY;
  let maxContextChars = Number.POSITIVE_INFINITY;
  if (slimContext) {
    memory = await loadSlimMemory(sessionId);
    if (!memory.summary && !memory.turns.length && request.sessionId) {
      seedSlimMemoryFromMessages(memory, await loadMessages(request.sessionId));
    }
    maxContextTokens = Math.max(150_000, Math.floor(Number(resolved.model.contextWindow ?? 128_000) * 0.6));
    maxContextChars = Math.max(8_000, maxContextTokens * 4);
    useFullContext = shouldUseFullContext(memory, maxContextTokens, maxContextChars);
    if (!useFullContext && memory.contextStage === "full") {
      // Stage one only drops native thinking/tool trajectories. Token usage from that native
      // request must not immediately trigger stage-two summarization.
      memory.contextStage = "slim";
      memory.contextTokens = 0;
      memory.fullMessages = [];
    }
    appendSlimTurn(memory, input.text);
    // Capacity decisions must use the rebuilt summary/prompt/conclusion context, not token usage
    // from the discarded native reasoning and tool trajectory.
    const rebuiltContextTokens = estimateContextTokens(formatSlimMemory(memory));
    const compacted = await compactSlimMemory(memory, async (earlier) => {
      const summaryPrompt = [
        "请把下面较早的会话记忆压缩成供另一个编码 Agent 使用的摘要。",
        "保留用户意图、决策、改动文件、关键标识、约束和未完成事项；不要照抄对话或添加评论。",
        "",
        earlier,
      ].join("\n");
      const summarizeWith = async (summaryModel) => {
        const summaryRuntime = await createAlkaidAgent({
          cwd: request.cwd,
          model: summaryModel.model,
          apiKey: summaryModel.apiKey,
          thinkingLevel: summaryModel.thinkingLevel ?? request.reasoningEffort,
        });
        let summary = "";
        summaryRuntime.agent.subscribe((event) => {
          if (event.type === "message_update" && event.assistantMessageEvent.type === "text_delta") {
            summary += event.assistantMessageEvent.delta;
          }
        });
        try {
          await summaryRuntime.agent.prompt(summaryPrompt);
          if (!summary.trim()) throw new Error("摘要模型没有返回内容");
          return summary;
        } finally {
          await summaryRuntime.close();
        }
      };
      let lightweight;
      try {
        lightweight = request.lightweightModel
          ? resolveAlkaidModel(config, request.lightweightModel)
          : resolved;
        return await summarizeWith(lightweight);
      } catch (error) {
        const currentWasAttempted = lightweight
          && lightweight.model.provider === resolved.model.provider
          && lightweight.model.id === resolved.model.id;
        if (currentWasAttempted) throw error;
        return summarizeWith(resolved);
      }
    }, {
      maxTurns: Number.POSITIVE_INFINITY,
      currentTokens: rebuiltContextTokens,
      maxTokens: maxContextTokens,
      maxChars: Number.POSITIVE_INFINITY,
    });
    if (compacted) memory.contextTokens = 0;
  }
  const stripsCompletedReasoning = slimContext
    && (resolved.model.api?.startsWith("openai") || resolved.model.api === "azure-openai-responses");
  let nativeMessages;
  if (!slimContext) nativeMessages = await loadMessages(request.sessionId);
  else if (memory.pendingMessages?.length) nativeMessages = memory.pendingMessages;
  else {
    nativeMessages = useFullContext ? memory.fullMessages : [];
    if (stripsCompletedReasoning) nativeMessages = stripCompletedOpenAIReasoning(nativeMessages);
  }
  // Persist the request before provider/agent initialization. The runtime must still receive the
  // previous transcript, otherwise the same request would be replayed once by history and once by
  // prompt(). If initialization or streaming fails, the next turn can resume from this checkpoint.
  const pendingMessages = messagesWithPendingAlkaidPrompt(nativeMessages, input);
  if (slimContext) {
    memory.pendingMessages = pendingMessages;
    await saveSlimMemory(sessionId, memory).catch((error) => {
      process.stderr.write(`Vega pending-turn persistence failed: ${error instanceof Error ? error.message : String(error)}\n`);
    });
  } else {
    await saveMessages(sessionId, pendingMessages).catch((error) => {
      process.stderr.write(`Vega pending-turn persistence failed: ${error instanceof Error ? error.message : String(error)}\n`);
    });
  }
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
  let activeTools = 0;
  const toolItems = new Map();
  let commandBusy = false;
  let commandRevision = 0;
  const steeringMessages = [];
  // Timeout diagnostics are paused together with the provider idle timeout. Keep the recorder and
  // payload below intact so both can be restored by changing the feature flag in alkaid-core.mjs.
  const diagnosticLog = ALKAID_PROVIDER_IDLE_TIMEOUT_ENABLED
    ? createAlkaidDiagnosticLog(dataRoot)
    : { record() {}, async flush() {} };
  const requestStartedAt = Date.now();
  let providerAttemptStartedAt = requestStartedAt;
  let providerAttempt = 0;
  let retryAttempt = 0;
  let attemptActivityBaseline = {};
  const providerActivity = {
    lastEvent: "request_created",
    messageStarts: 0,
    messageEnds: 0,
    textDeltas: 0,
    thinkingDeltas: 0,
    textChars: 0,
    thinkingChars: 0,
    toolStarts: 0,
    toolEnds: 0,
  };
  const idleTimeout = createAlkaidIdleTimeout({
    timeoutMs: ALKAID_PROVIDER_IDLE_TIMEOUT_ENABLED ? ALKAID_PROVIDER_IDLE_TIMEOUT_MS : 0,
    onTimeout: () => {
      const messages = runtime.agent.state.messages;
      diagnosticLog.record({
        timestamp: new Date().toISOString(),
        event: "provider_stream_idle_timeout",
        timeoutMs: ALKAID_PROVIDER_IDLE_TIMEOUT_MS,
        requestElapsedMs: Date.now() - requestStartedAt,
        attemptElapsedMs: Date.now() - providerAttemptStartedAt,
        sessionId,
        provider: resolved.model.provider,
        model: resolved.model.id,
        api: resolved.model.api,
        endpoint: alkaidDiagnosticEndpoint(resolved.model.baseUrl),
        thinkingLevel: resolved.thinkingLevel ?? request.reasoningEffort ?? null,
        providerAttempt,
        retryAttempt,
        activity: { ...providerActivity },
        attemptActivity: Object.fromEntries(Object.entries(providerActivity).map(([key, value]) => [
          key,
          typeof value === "number" ? value - (attemptActivityBaseline[key] ?? 0) : value,
        ])),
        activeTools,
        commandBusy,
        queuedSteeringMessages: steeringMessages.length,
        messageCount: messages.length,
        messageTail: messages.slice(-8).map((message) => ({
          role: message.role,
          stopReason: message.stopReason ?? null,
          contentParts: Array.isArray(message.content) ? message.content.length : 0,
          contentTypes: Array.isArray(message.content)
            ? message.content.map((part) => part?.type ?? "unknown")
            : [],
          hasError: Boolean(message.errorMessage),
        })),
        process: { pid: process.pid, node: process.version, platform: process.platform, arch: process.arch },
      });
      runtime.agent.abort();
    },
  });
  const settlePendingInput = async () => {
    // stdin delivery and the provider's final event can race. Require one quiet interval after
    // the most recently processed command so a late steer is visible to hasQueuedMessages().
    let observedRevision;
    do {
      observedRevision = commandRevision;
      await new Promise((resolve) => setTimeout(resolve, 25));
    } while (commandBusy || commandRevision !== observedRevision);
  };
  runtime.agent.subscribe((event) => {
    if (event.type === "message_end" && event.message.role === "assistant") {
      providerActivity.lastEvent = "message_end";
      providerActivity.messageEnds += 1;
      usage = mergeAlkaidUsage(usage, event.message.usage);
    }
    if (event.type === "message_start" && event.message.role === "assistant") {
      attemptActivityBaseline = { ...providerActivity };
      providerActivity.lastEvent = "message_start";
      providerActivity.messageStarts += 1;
      providerAttempt += 1;
      providerAttemptStartedAt = Date.now();
      idleTimeout.touch();
      text = "";
      thinking = "";
      if (reuseAssistantIds) {
        reuseAssistantIds = false;
      } else {
        assistantId = `assistant-${randomUUID()}`;
        thinkingId = `thinking-${randomUUID()}`;
      }
    } else if (event.type === "message_update" && event.assistantMessageEvent.type === "text_delta") {
      providerActivity.lastEvent = "text_delta";
      providerActivity.textDeltas += 1;
      providerActivity.textChars += event.assistantMessageEvent.delta.length;
      idleTimeout.touch();
      text += event.assistantMessageEvent.delta;
      send({ type: "item", item: { id: assistantId, type: "agent_message", text } });
    } else if (event.type === "message_update" && event.assistantMessageEvent.type === "thinking_delta") {
      providerActivity.lastEvent = "thinking_delta";
      providerActivity.thinkingDeltas += 1;
      providerActivity.thinkingChars += event.assistantMessageEvent.delta.length;
      idleTimeout.touch();
      thinking += event.assistantMessageEvent.delta;
      send({ type: "item", item: { id: thinkingId, type: "reasoning", text: thinking } });
    } else if (event.type === "tool_execution_start") {
      providerActivity.lastEvent = "tool_execution_start";
      providerActivity.toolStarts += 1;
      activeTools += 1;
      idleTimeout.pause();
      const item = startedToolItem(event);
      toolItems.set(event.toolCallId, item);
      send({ type: "item", item });
    } else if (event.type === "tool_execution_end") {
      providerActivity.lastEvent = "tool_execution_end";
      providerActivity.toolEnds += 1;
      activeTools = Math.max(0, activeTools - 1);
      if (activeTools === 0) idleTimeout.resume();
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
        commandBusy = true;
        try {
          const message = await alkaidUserMessage(command.parts);
          const textPart = message.content.find((part) => part.type === "text");
          if (textPart) {
            if (slimContext) appendSlimTurn(memory, textPart.text);
            textPart.text = await expandAlkaidSkillCommand(textPart.text, runtime.skills);
          }
          runtime.agent.steer(message);
          steeringMessages.push(message);
          commandRevision += 1;
        } finally {
          commandBusy = false;
        }
      }
    }
  })().catch((error) => send({ type: "error", message: error instanceof Error ? error.message : String(error) }));
  try {
    send({ type: "ready", sessionId });
    const expandedText = await expandAlkaidSkillCommand(input.text, runtime.skills);
    const promptText = slimContext && !useFullContext ? messageWithSlimMemory(expandedText, memory) : expandedText;
    const outcome = await runAlkaidPromptWithRetry(runtime.agent, promptText, input.images, {
      isCancelled: () => cancelled,
      runAttempt: (operation) => idleTimeout.run(operation),
      settlePendingInput,
      prepareRetry: () => restoreAlkaidSteeringForRetry(runtime.agent, steeringMessages),
      onRetry: ({ attempt, error }) => {
        retryAttempt = attempt;
        send({
          type: "timing",
          phase: "provider_retry",
          elapsedMs: 0,
          attempt,
          reason: error instanceof Error ? error.message : String(error),
        });
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
            const completedMessages = structuredClone(runtime.agent.state.messages);
            memory.fullMessages = stripsCompletedReasoning
              ? stripCompletedOpenAIReasoning(completedMessages)
              : completedMessages;
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
  } catch (error) {
    // Keep the exact native trajectory when one exists; the pre-initialization checkpoint above
    // remains available when agent creation itself failed before this try block was entered.
    const failedMessages = runtime.agent.state.messages.length > nativeMessages.length
      ? runtime.agent.state.messages
      : pendingMessages;
    if (slimContext) {
      memory.pendingMessages = structuredClone(failedMessages);
      await saveSlimMemory(sessionId, memory).catch((saveError) => {
        process.stderr.write(`Vega failed-turn persistence failed: ${saveError instanceof Error ? saveError.message : String(saveError)}\n`);
      });
    } else {
      await saveMessages(sessionId, failedMessages).catch((saveError) => {
        process.stderr.write(`Vega failed-turn persistence failed: ${saveError instanceof Error ? saveError.message : String(saveError)}\n`);
      });
    }
    throw error;
  } finally {
    if (slimContext) await saveSlimMemory(sessionId, memory).catch(() => {});
    await diagnosticLog.flush();
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
