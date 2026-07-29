export function createSlimMemory() {
  return {
    digests: [],
    preservedUserPrompts: [],
    turns: [],
    pendingMessages: [],
    fullMessages: [],
    contextTokens: 0,
    contextStage: "full",
    contextTier: "normal",
    rewriteVersion: 0,
    systemPromptSnapshot: "",
    systemFingerprint: "",
    systemPromptHash: "",
    toolSchemaHash: "",
    lastShapeRewriteVersion: 0,
    consecutiveCompactions: 0,
    compactStuck: false,
  };
}

function textContent(content) {
  if (typeof content === "string") return content.trim();
  if (!Array.isArray(content)) return "";
  return content
    .filter((part) => part?.type === "text")
    .map((part) => String(part.text ?? "").trim())
    .filter(Boolean)
    .join("\n");
}

export function appendSlimTurn(memory, userPrompt) {
  const prompt = String(userPrompt ?? "").trim();
  if (prompt) memory.turns.push({ userPrompts: [prompt], conclusion: "" });
  return memory;
}

export function setLatestConclusion(memory, content) {
  const conclusion = textContent(content);
  if (!conclusion) return memory;
  let turn = memory.turns.at(-1);
  if (!turn || turn.conclusion) {
    turn = { userPrompts: [], conclusion: "" };
    memory.turns.push(turn);
  }
  turn.conclusion = conclusion;
  return memory;
}

/**
 * A cancelled turn has no conclusion. Its prompts belong to the next completed conclusion and
 * must remain together, so interruptions can leave several verbatim user prompts in one turn.
 */
export function normalizeSlimMemory(memory) {
  const normalized = [];
  let pendingPrompts = [];
  for (const raw of memory.turns ?? []) {
    const prompts = Array.isArray(raw?.userPrompts)
      ? raw.userPrompts.map(String).map((value) => value.trim()).filter(Boolean)
      : [String(raw?.userPrompt ?? "").trim()].filter(Boolean);
    pendingPrompts.push(...prompts);
    const conclusion = String(raw?.conclusion ?? "").trim();
    if (conclusion) {
      normalized.push({ userPrompts: pendingPrompts, conclusion });
      pendingPrompts = [];
    }
  }
  if (pendingPrompts.length) normalized.push({ userPrompts: pendingPrompts, conclusion: "" });
  const legacySummary = String(memory.summary ?? "").trim();
  memory.digests = Array.isArray(memory.digests)
    ? memory.digests.map(String).map((value) => value.trim()).filter(Boolean)
    : legacySummary ? [legacySummary] : [];
  memory.preservedUserPrompts = Array.isArray(memory.preservedUserPrompts)
    ? memory.preservedUserPrompts.map(String).map((value) => value.trim()).filter(Boolean)
    : [];
  memory.turns = normalized;
  delete memory.summary;
  return memory;
}

export function memoryWithoutCurrent(memory, { pendingMessages = false } = {}) {
  const normalized = normalizeSlimMemory({
    digests: structuredClone(memory.digests ?? []),
    preservedUserPrompts: structuredClone(memory.preservedUserPrompts ?? []),
    turns: structuredClone(memory.turns ?? []),
  });
  const latest = normalized.turns.at(-1);
  if (latest && !latest.conclusion) {
    // An interrupted turn is supplied as native PI messages so its user prompts, assistant
    // messages, and tool results stay together. Otherwise only omit the new current prompt.
    if (pendingMessages) normalized.turns.pop();
    else latest.userPrompts.pop();
  }
  if (latest && !latest.userPrompts.length && !latest.conclusion) normalized.turns.pop();
  return normalized;
}

export function formatSlimMemory(memory) {
  const normalized = normalizeSlimMemory(memory);
  const sections = [
    "请使用下面的只追加会话记录继续工作。不要要求用户重复之前的要求。",
    "",
    "## Conversation",
  ];
  if (normalized.preservedUserPrompts.length) {
    sections.push("", "### Preserved user requests");
    normalized.preservedUserPrompts.forEach((prompt) => sections.push(`User:\n${prompt}`));
  }
  if (normalized.digests.length) {
    sections.push("", "### Frozen digests");
    normalized.digests.forEach((digest, index) => sections.push(`Digest ${index + 1}:\n${digest}`));
  }
  for (const turn of normalized.turns) {
    for (const prompt of turn.userPrompts) sections.push("", `User:\n${prompt}`);
    if (turn.conclusion) sections.push(`Assistant:\n${turn.conclusion}`);
  }
  return sections.join("\n");
}

export async function compactSlimMemory(
  memory,
  summarize,
  {
    maxTurns = Number.POSITIVE_INFINITY,
    maxChars = Number.POSITIVE_INFINITY,
    currentTokens = 0,
    maxTokens = Number.POSITIVE_INFINITY,
  } = {},
) {
  normalizeSlimMemory(memory);
  const formatted = formatSlimMemory({ digests: memory.digests, turns: structuredClone(memory.turns) });
  const withinTurnLimit = memory.turns.length <= maxTurns;
  const belowCharacterLimit = !Number.isFinite(maxChars) || formatted.length < maxChars;
  const belowTokenLimit = !Number.isFinite(maxTokens) || currentTokens < maxTokens;
  if (withinTurnLimit && belowCharacterLimit && belowTokenLimit) {
    memory.consecutiveCompactions = 0;
    memory.compactStuck = false;
    return false;
  }
  if (memory.compactStuck || memory.turns.length < 2) return false;
  if ((memory.consecutiveCompactions ?? 0) >= 2) {
    memory.compactStuck = true;
    return false;
  }

  // Preserve the latest completed turn, or the latest completion plus following interrupted
  // prompts. Older frozen digests never participate in a replacement summary.
  const protectedCount = memory.turns.at(-1)?.conclusion ? 1 : Math.min(2, memory.turns.length);
  const split = memory.turns.length - protectedCount;
  if (split <= 0) return false;

  const compactedTurns = memory.turns.slice(0, split);
  const preservedPrompts = compactedTurns
    .flatMap((turn) => turn.userPrompts ?? [])
    .filter((prompt) => prompt.length <= 2_000 && !memory.preservedUserPrompts.includes(prompt));
  const earlier = {
    digests: [],
    preservedUserPrompts: [],
    turns: compactedTurns.map((turn) => ({ userPrompts: turn.userPrompts, conclusion: turn.conclusion })),
  };
  const digest = String(await summarize(formatSlimMemory(earlier)) ?? "").trim();
  if (!digest) return false;
  memory.preservedUserPrompts.push(...preservedPrompts);
  memory.digests.push(digest);
  memory.turns = memory.turns.slice(split);
  memory.rewriteVersion = (memory.rewriteVersion ?? 0) + 1;
  memory.consecutiveCompactions = (memory.consecutiveCompactions ?? 0) + 1;
  return true;
}

/**
 * Completed OpenAI turns do not need to replay exposed reasoning summaries/state. Responses tool
 * calls also carry an item id paired with that reasoning item, so drop the item-id suffix while
 * retaining the call id used by the matching tool result. Call this only for completed turns;
 * interrupted/native trajectories must remain untouched.
 */
export function stripCompletedOpenAIReasoning(messages) {
  return (messages ?? []).map((message) => {
    if (message?.role !== "assistant" || !Array.isArray(message.content)) return message;
    let changed = false;
    const content = [];
    for (const block of message.content) {
      if (block?.type === "thinking") {
        changed = true;
        continue;
      }
      if (block?.type === "toolCall" && typeof block.id === "string" && block.id.includes("|")) {
        changed = true;
        content.push({ ...block, id: block.id.split("|", 1)[0] });
      } else {
        content.push(block);
      }
    }
    return changed ? { ...message, content } : message;
  });
}

export function estimateContextTokens(text) {
  let ascii = 0;
  let nonAscii = 0;
  for (const char of String(text ?? "")) {
    if (char.codePointAt(0) <= 0x7f) ascii += 1;
    else nonAscii += 1;
  }
  // Common provider tokenizers average roughly four ASCII characters per token, while CJK and
  // other non-ASCII text are commonly close to one token per character. This is intentionally
  // conservative when the provider has not reported usage for the rebuilt context yet.
  return Math.ceil(ascii / 4) + nonAscii;
}

export function contextTokensFromMessages(messages) {
  let tokens = 0;
  for (const message of messages ?? []) {
    if (message?.role !== "assistant" || !message.usage) continue;
    const usage = message.usage;
    // Each assistant request reports the context size at that point. The latest/largest request,
    // not the sum across tool calls, is the value that should be compared with the context window.
    const total = Number(usage.totalTokens ?? usage.total_tokens) || 0;
    const output = Number(usage.output ?? usage.outputTokens ?? usage.output_tokens) || 0;
    const input = Number(usage.input) || 0;
    const cached = (Number(usage.cacheRead) || 0) + (Number(usage.cacheWrite) || 0);
    // Some providers include cached tokens in `input`, while others report them separately.
    const measured = total > output ? total - output : (input >= cached ? input : input + cached);
    tokens = Math.max(tokens, measured);
  }
  return tokens;
}

export function contextPressureTier(currentTokens, contextWindow) {
  if (!(currentTokens > 0) || !(contextWindow > 0)) return "normal";
  const ratio = currentTokens / contextWindow;
  if (ratio >= 0.9) return "force";
  if (ratio >= 0.8) return "elide";
  if (ratio >= 0.6) return "snip";
  if (ratio >= 0.5) return "warn";
  return "normal";
}

function compactToolText(text, tier, toolCallId) {
  const value = String(text ?? "");
  if (value.includes("[elided tool result")) return value;
  if (tier === "snip" && Buffer.byteLength(value, "utf8") <= 8 * 1024) return value;
  const bytes = Buffer.byteLength(value, "utf8");
  const id = toolCallId ? ` ${toolCallId}` : "";
  if (tier === "elide" || tier === "force") {
    return `[elided tool result${id} — ${bytes} bytes; re-run the tool if needed]`;
  }
  const head = value.slice(0, 3_000);
  const tail = value.slice(-2_000);
  return `${head}\n\n…[snipped older tool result${id} — ${bytes} bytes]…\n\n${tail}`;
}

/** Apply one-way, stable compaction to older native tool results before summary is necessary. */
export function compactNativeToolResults(messages, tier, { preserveRecent = 6 } = {}) {
  if (!["snip", "elide", "force"].includes(tier)) return { messages, changed: false };
  const cutoff = Math.max(0, (messages?.length ?? 0) - preserveRecent);
  let changed = false;
  const next = (messages ?? []).map((message, index) => {
    if (index >= cutoff || message?.role !== "toolResult" || !Array.isArray(message.content)) return message;
    let contentChanged = false;
    const content = message.content.map((part) => {
      if (part?.type !== "text") return part;
      const text = compactToolText(part.text, tier, message.toolCallId);
      if (text === part.text) return part;
      contentChanged = true;
      return { ...part, text };
    });
    if (!contentChanged) return message;
    changed = true;
    return { ...message, content };
  });
  return { messages: changed ? next : messages, changed };
}

export function shouldUseFullContext(memory, maxContextTokens, maxContextChars = Number.POSITIVE_INFINITY) {
  if (memory.pendingMessages?.length) return true;
  if (memory.contextStage === "slim") return false;
  if (!(memory.fullMessages?.length > 0)) return (memory.turns?.length ?? 0) === 0;
  const measuredTokens = memory.contextTokens ?? 0;
  return measuredTokens > 0
    ? measuredTokens < maxContextTokens
    : JSON.stringify(memory.fullMessages).length < maxContextChars;
}

/**
 * Replace completed native history with canonical slim memory while retaining the active user
 * turn and every assistant/tool message produced after it. This mirrors Reasonix's forced fold:
 * completed history is the cache-reset region, while in-flight work remains verbatim.
 */
export function rebaseNativeContextForSlimMemory(messages, activeTurnStart, memory) {
  const source = messages ?? [];
  const start = Number.isInteger(activeTurnStart) ? activeTurnStart : -1;
  if (start <= 0 || start >= source.length || source[start]?.role !== "user") {
    return { messages: source, changed: false };
  }
  const compactContext = formatSlimMemory(memoryWithoutCurrent(memory, { pendingMessages: true }));
  const current = structuredClone(source[start]);
  if (typeof current.content === "string") {
    current.content = `${compactContext}\n\nUser:\n${current.content}`;
  } else if (Array.isArray(current.content)) {
    const firstText = current.content.findIndex((part) => part?.type === "text");
    if (firstText >= 0) {
      current.content[firstText] = {
        ...current.content[firstText],
        text: `${compactContext}\n\nUser:\n${String(current.content[firstText].text ?? "")}`,
      };
    } else {
      current.content.unshift({ type: "text", text: compactContext });
    }
  } else {
    current.content = [{ type: "text", text: compactContext }];
  }
  return { messages: [current, ...structuredClone(source.slice(start + 1))], changed: true };
}

export function seedSlimMemoryFromMessages(memory, messages) {
  for (const message of messages ?? []) {
    if (message?.role === "user") appendSlimTurn(memory, textContent(message.content));
    else if (message?.role === "assistant" && message.stopReason !== "error") {
      setLatestConclusion(memory, message.content);
    }
  }
  memory.fullMessages = structuredClone(messages ?? []);
  memory.contextTokens = contextTokensFromMessages(messages);
  memory.contextStage = "full";
  return normalizeSlimMemory(memory);
}
