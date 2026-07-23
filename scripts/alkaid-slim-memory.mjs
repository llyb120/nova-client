export const VEGA_SLIM_MEMORY_TURNS = 20;

export function createSlimMemory() {
  return { summary: "", turns: [], pendingMessages: [] };
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
  memory.turns = normalized;
  memory.summary = String(memory.summary ?? "").trim();
  return memory;
}

export function memoryWithoutCurrent(memory, { pendingMessages = false } = {}) {
  const normalized = normalizeSlimMemory({
    summary: memory.summary,
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
  const sections = [];
  if (normalized.summary) sections.push("## 更早轮次摘要", normalized.summary);
  if (normalized.turns.length) sections.push("## 最近轮次");
  normalized.turns.forEach((turn, index) => {
    sections.push(`### 轮次 ${index + 1}`);
    for (const prompt of turn.userPrompts) sections.push(`用户提示：${prompt}`);
    if (turn.conclusion) sections.push(`结论：${turn.conclusion}`);
  });
  return sections.join("\n");
}

export async function compactSlimMemory(
  memory,
  summarize,
  { maxTurns = VEGA_SLIM_MEMORY_TURNS, maxChars = Number.POSITIVE_INFINITY } = {},
) {
  normalizeSlimMemory(memory);
  const formatted = formatSlimMemory({ summary: memory.summary, turns: structuredClone(memory.turns) });
  if (memory.turns.length <= maxTurns && formatted.length <= maxChars) return false;

  // The latest conclusion and every prompt after it are invariant. Prefer retaining up to 20
  // complete recent turns; if the model limit is already exceeded, summarize all older turns.
  const protectedCount = memory.turns.at(-1)?.conclusion ? 1 : Math.min(2, memory.turns.length);
  // Match Cursor's policy: once the threshold is crossed, summarize every older complete turn
  // rather than repeatedly shaving off a single turn. The newest conclusion (or the newest
  // conclusion plus all following interrupted prompts) remains verbatim.
  const split = memory.turns.length - protectedCount;
  if (split <= 0) return false;

  const earlier = { summary: memory.summary, turns: memory.turns.slice(0, split) };
  const summary = String(await summarize(formatSlimMemory(earlier)) ?? "").trim();
  if (!summary) return false;
  memory.summary = summary;
  memory.turns = memory.turns.slice(split);
  return true;
}

export function seedSlimMemoryFromMessages(memory, messages) {
  for (const message of messages ?? []) {
    if (message?.role === "user") appendSlimTurn(memory, textContent(message.content));
    else if (message?.role === "assistant" && message.stopReason !== "error") {
      setLatestConclusion(memory, message.content);
    }
  }
  return normalizeSlimMemory(memory);
}
