import { dataRoot, loadMessages, mcpServers, runAlkaidBridge, saveJson, saveMessages, send, sessionPath, startedToolItem } from "./alkaid-bridge-common.mjs";
import { createHash, randomUUID } from "node:crypto";
import { readFile } from "node:fs/promises";
import { ALKAID_PROVIDER_IDLE_TIMEOUT_ENABLED, ALKAID_PROVIDER_IDLE_TIMEOUT_MS, alkaidPromptInput, alkaidUserMessage, createAlkaidAgent, createAlkaidIdleTimeout, expandAlkaidSkillCommand, mergeAlkaidUsage, messagesWithPendingAlkaidPrompt, restoreAlkaidSteeringForRetry, runAlkaidPromptWithRetry } from "./alkaid-core.mjs";
import { alkaidDiagnosticEndpoint, createAlkaidDiagnosticLog } from "./alkaid-diagnostics.mjs";
import { appendSlimTurn, compactNativeToolResults, compactSlimMemory, contextPressureTier, contextTokensFromMessages, createSlimMemory, estimateContextTokens, formatSlimMemory, memoryWithoutCurrent, rebaseNativeContextForSlimMemory, seedSlimMemoryFromMessages, setLatestConclusion, shouldUseFullContext, stripCompletedOpenAIReasoning } from "./alkaid-slim-memory.mjs";
import { loadAlkaidConfig, resolveAlkaidModel } from "./alkaid-config.mjs";

function stableHash(value) {
  return createHash("sha256")
    .update(typeof value === "string" ? value : JSON.stringify(value))
    .digest("hex")
    .slice(0, 16);
}

function slimMemoryPath(sessionId) {
  return sessionPath(sessionId).replace(/\.json$/, ".slim.json");
}

async function loadSlimMemory(sessionId) {
  if (!sessionId) return createSlimMemory();
  try {
    const parsed = JSON.parse(await readFile(slimMemoryPath(sessionId), "utf8"));
    return Array.isArray(parsed?.turns)
      ? {
          ...createSlimMemory(),
          digests: Array.isArray(parsed.digests)
            ? parsed.digests.map(String).filter(Boolean)
            : String(parsed.summary ?? "").trim() ? [String(parsed.summary).trim()] : [],
          preservedUserPrompts: Array.isArray(parsed.preservedUserPrompts)
            ? parsed.preservedUserPrompts.map(String).filter(Boolean)
            : [],
          turns: parsed.turns,
          pendingMessages: Array.isArray(parsed.pendingMessages) ? parsed.pendingMessages : [],
          fullMessages: Array.isArray(parsed.fullMessages) ? parsed.fullMessages : [],
          contextTokens: Number(parsed.contextTokens) || 0,
          contextStage: parsed.contextStage === "slim" ? "slim" : "full",
          contextTier: ["normal", "warn", "snip", "elide", "force"].includes(parsed.contextTier)
            ? parsed.contextTier
            : "normal",
          rewriteVersion: Number(parsed.rewriteVersion) || 0,
          systemPromptSnapshot: String(parsed.systemPromptSnapshot ?? ""),
          systemFingerprint: String(parsed.systemFingerprint ?? ""),
          systemPromptHash: String(parsed.systemPromptHash ?? ""),
          toolSchemaHash: String(parsed.toolSchemaHash ?? ""),
          lastShapeRewriteVersion: Number(parsed.lastShapeRewriteVersion) || 0,
          consecutiveCompactions: Number(parsed.consecutiveCompactions) || 0,
          compactStuck: parsed.compactStuck === true,
        }
      : createSlimMemory();
  } catch {
    return createSlimMemory();
  }
}

async function saveSlimMemory(sessionId, memory) {
  await saveJson(slimMemoryPath(sessionId), { version: 3, ...memory });
}

function messageWithSlimMemory(text, memory) {
  const context = formatSlimMemory(memoryWithoutCurrent(memory, {
    pendingMessages: memory.pendingMessages?.length > 0,
  }));
  return `${context}\n\nUser:\n${text}`;
}


async function prompt(request, commands) {
  const input = await alkaidPromptInput(request.parts);
  const config = await loadAlkaidConfig({ root: dataRoot, serverConfig: request.alkaidServerConfig });
  const resolved = resolveAlkaidModel(config, request.model);
  const sessionId = request.sessionId || randomUUID();
  // Reasonix-style context is Vega's canonical session model, not an optional request mode.
  const slimContext = true;
  let memory = createSlimMemory();
  let useFullContext = false;
  let maxContextTokens = Number.POSITIVE_INFINITY;
  let maxContextChars = Number.POSITIVE_INFINITY;
  let contextWindow = Number.POSITIVE_INFINITY;
  if (slimContext) {
    memory = await loadSlimMemory(sessionId);
    if (!memory.digests.length && !memory.turns.length && request.sessionId) {
      seedSlimMemoryFromMessages(memory, await loadMessages(request.sessionId));
    }
    const systemFingerprint = stableHash({ cwd: request.cwd, mode: request.mode === "plan" ? "plan" : "agent" });
    if (memory.systemFingerprint && memory.systemFingerprint !== systemFingerprint) {
      memory.systemPromptSnapshot = "";
      memory.rewriteVersion = (memory.rewriteVersion ?? 0) + 1;
    }
    memory.systemFingerprint = systemFingerprint;

    contextWindow = Math.max(2_000, Number(resolved.model.contextWindow ?? 128_000));
    maxContextTokens = Math.max(2_000, Math.floor(contextWindow * 0.75));
    const forceContextTokens = Math.max(2_000, Math.floor(contextWindow * 0.9));
    maxContextChars = Math.max(8_000, forceContextTokens * 4);
    const pressure = contextPressureTier(memory.contextTokens, contextWindow);
    if (memory.contextStage === "full" && ["snip", "elide", "force"].includes(pressure)) {
      const compactedTools = compactNativeToolResults(memory.fullMessages, pressure);
      if (compactedTools.changed) {
        memory.fullMessages = compactedTools.messages;
        memory.rewriteVersion = (memory.rewriteVersion ?? 0) + 1;
      }
    }
    memory.contextTier = pressure;
    useFullContext = shouldUseFullContext(memory, forceContextTokens, maxContextChars);
    if (!useFullContext && memory.contextStage === "full") {
      memory.contextStage = "slim";
      memory.contextTokens = 0;
      memory.fullMessages = [];
      memory.rewriteVersion = (memory.rewriteVersion ?? 0) + 1;
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
  const resumedPendingTurn = slimContext && memory.pendingMessages?.length > 0;
  if (!slimContext) nativeMessages = await loadMessages(request.sessionId);
  else if (resumedPendingTurn) nativeMessages = memory.pendingMessages;
  else {
    nativeMessages = useFullContext ? memory.fullMessages : [];
    if (stripsCompletedReasoning) nativeMessages = stripCompletedOpenAIReasoning(nativeMessages);
  }
  // A recovered interrupted trajectory is itself active work and must not be folded as completed
  // history. Fresh turns may fold everything before their newly appended user message.
  let activeTurnStart = resumedPendingTurn ? 0 : nativeMessages.length;
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
    systemPromptSnapshot: memory.systemPromptSnapshot,
    // Early turns and interrupted work retain the native message/tool trajectory. Once either
    // threshold is reached, compact memory replaces completed trajectories as usual.
    messages: nativeMessages,
    prepareNextTurnWithContext: slimContext
      ? async ({ message, toolResults, context }) => {
          // Reasonix performs context maintenance after every model/tool round, before the next
          // provider request. Waiting for agent_end lets a long single task overflow mid-turn.
          if (!toolResults?.length) return undefined;
          const measuredTokens = contextTokensFromMessages([message]);
          if (!(measuredTokens > 0)) return undefined;
          const pressure = contextPressureTier(measuredTokens, contextWindow);
          memory.contextTokens = measuredTokens;
          memory.contextTier = pressure;
          let messages = context.messages;
          let changed = false;

          if (["snip", "elide", "force"].includes(pressure)) {
            const compactedTools = compactNativeToolResults(messages, pressure);
            if (compactedTools.changed) {
              messages = compactedTools.messages;
              changed = true;
            }
          }
          if (pressure === "force" && activeTurnStart > 0) {
            const rebased = rebaseNativeContextForSlimMemory(messages, activeTurnStart, memory);
            if (rebased.changed) {
              messages = rebased.messages;
              activeTurnStart = 0;
              memory.contextStage = "slim";
              memory.fullMessages = [];
              memory.contextTokens = estimateContextTokens(formatSlimMemory(memory));
              changed = true;
            }
          }
          if (!changed) return undefined;

          memory.rewriteVersion = (memory.rewriteVersion ?? 0) + 1;
          memory.pendingMessages = structuredClone(messages);
          await saveSlimMemory(sessionId, memory).catch((error) => {
            process.stderr.write(`Vega mid-turn context persistence failed: ${error instanceof Error ? error.message : String(error)}\n`);
          });
          send({
            type: "timing",
            phase: "mid_turn_context_rewrite",
            elapsedMs: 0,
            contextTier: pressure,
            measuredTokens,
            rewriteVersion: memory.rewriteVersion,
          });
          return { context: { ...context, messages } };
        }
      : undefined,
    readOnly: request.mode === "plan",
  });
  if (!memory.systemPromptSnapshot) memory.systemPromptSnapshot = runtime.systemPrompt;
  const systemPromptHash = stableHash(runtime.systemPrompt);
  const toolSchemaHash = stableHash(runtime.toolShape);
  const cacheMissReasons = [];
  if (memory.systemPromptHash && memory.systemPromptHash !== systemPromptHash) cacheMissReasons.push("system_changed");
  if (memory.toolSchemaHash && memory.toolSchemaHash !== toolSchemaHash) cacheMissReasons.push("tools_changed");
  if ((memory.lastShapeRewriteVersion ?? 0) !== (memory.rewriteVersion ?? 0)) cacheMissReasons.push("history_rewritten");
  memory.systemPromptHash = systemPromptHash;
  memory.toolSchemaHash = toolSchemaHash;
  memory.lastShapeRewriteVersion = memory.rewriteVersion ?? 0;
  send({
    type: "timing",
    phase: "context_shape",
    elapsedMs: 0,
    contextTier: memory.contextTier,
    rewriteVersion: memory.rewriteVersion ?? 0,
    cacheMissReasons,
    systemPromptHash,
    toolSchemaHash,
    historyHash: stableHash(formatSlimMemory(memoryWithoutCurrent(memory, {
      pendingMessages: memory.pendingMessages?.length > 0,
    }))),
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
          const forceContextTokens = Math.max(2_000, Math.floor(contextWindow * 0.9));
          const belowCapacity = measuredTokens > 0
            ? measuredTokens < forceContextTokens
            : JSON.stringify(runtime.agent.state.messages).length < maxContextChars;
          if (belowCapacity) {
            const completedMessages = structuredClone(runtime.agent.state.messages);
            const strippedMessages = stripsCompletedReasoning
              ? stripCompletedOpenAIReasoning(completedMessages)
              : completedMessages;
            const pressure = contextPressureTier(measuredTokens, contextWindow);
            const compactedTools = compactNativeToolResults(strippedMessages, pressure);
            memory.fullMessages = compactedTools.messages;
            memory.contextTier = pressure;
            if (compactedTools.changed) memory.rewriteVersion = (memory.rewriteVersion ?? 0) + 1;
          } else {
            // 90% is the hard boundary: start a compact frozen-digest epoch before overflow.
            memory.contextStage = "slim";
            memory.contextTier = "force";
            memory.contextTokens = 0;
            memory.fullMessages = [];
            memory.rewriteVersion = (memory.rewriteVersion ?? 0) + 1;
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
    if (usage) {
      const inputTokens = Number(usage.input) || 0;
      const cacheReadTokens = Number(usage.cacheRead) || 0;
      const cacheWriteTokens = Number(usage.cacheWrite) || 0;
      const cacheDenominator = inputTokens >= cacheReadTokens + cacheWriteTokens
        ? inputTokens
        : inputTokens + cacheReadTokens + cacheWriteTokens;
      send({
        type: "timing",
        phase: "cache_shape",
        elapsedMs: 0,
        inputTokens,
        cacheReadTokens,
        cacheWriteTokens,
        cacheHitRate: cacheDenominator > 0 ? cacheReadTokens / cacheDenominator : 0,
        rewriteVersion: memory.rewriteVersion ?? 0,
      });
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

export function runContextBridge() {
  return runAlkaidBridge(prompt);
}
