import { createHash, randomUUID } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { extname, join } from "node:path";
import { createInterface } from "node:readline";
import { Agent } from "@cursor/sdk";
import { completePendingTools, createMessageState, cursorModelOptions, cursorShellProgram, cursorTodoPlan, isEditFilesTool, mapDelta, mapMessage, modelOptions, modelSelection } from "./cursor-bridge-common.mjs";
import { createCursorFilesystemTools, cursorPromptPrefix } from "./cursor-filesystem-tools.mjs";

const send = (message) => process.stdout.write(`${JSON.stringify(message)}\n`);
const TERMINAL_RUN_STATUSES = new Set(["completed", "finished", "error", "failed", "cancelled", "expired"]);
const CURSOR_STARTUP_TIMEOUT_MS = positiveInteger(process.env.NOVA_CURSOR_STARTUP_TIMEOUT_MS, 120_000);
const CURSOR_RECOVERY_TIMEOUT_MS = positiveInteger(process.env.NOVA_CURSOR_RECOVERY_TIMEOUT_MS, 15_000);
const CURSOR_SILENT_RETRIES = positiveInteger(process.env.NOVA_CURSOR_SILENT_RETRIES, 2);
const CURSOR_SILENT_RETRY_DELAYS_MS = [1_000, 3_000];
const CURSOR_CREATE_RETRY_DELAYS_MS = [1_000, 3_000, 7_000];
const CURSOR_RECOVERY_CONTEXT_CHARS = positiveInteger(process.env.NOVA_CURSOR_RECOVERY_CONTEXT_CHARS, 24_000);
const CURSOR_DEFAULT_CONTEXT_WINDOW = positiveInteger(process.env.NOVA_CURSOR_CONTEXT_WINDOW, 128_000);
const CURSOR_MODEL_CONTEXT_RULES = parseCursorModelContextRules(process.env.NOVA_CURSOR_MODEL_CONTEXTS);
const CURSOR_TRACE_TOOL_OUTPUT_MAX_CHARS = 32 * 1024;
const CURSOR_SLIM_MEMORY_DIR = process.env.NOVA_CURSOR_SLIM_MEMORY_DIR
  || join(process.env.NOVA_DATA_DIR || join(homedir(), ".nova"), "cursor-slim-memory");
const CURSOR_USER_DIR = process.env.NOVA_CURSOR_USER_DIR || join(homedir(), ".cursor");
const NOVA_DENY_TASK_SCRIPT = "nova-deny-task.mjs";
const NOVA_DENY_TASK_MARKER = "nova-deny-task";
const NOVA_DENY_TASK_SCRIPT_SOURCE = `process.stdout.write(JSON.stringify({
  permission: "deny",
  user_message: "Task / subagent tool is disabled by Nova.",
  agent_message: "The Task tool is disabled by Nova global Cursor hooks. Do the work yourself with the available tools (Shell, Read, Write, Grep, etc.) instead of spawning a subagent."
}));
`;

function stableHash(value) {
  return createHash("sha256")
    .update(typeof value === "string" ? value : JSON.stringify(value))
    .digest("hex")
    .slice(0, 16);
}

function parseCursorModelContextRules(value) {
  try {
    const parsed = JSON.parse(value || "[]");
    if (!Array.isArray(parsed)) return [];
    return parsed
      .map((rule) => ({
        prefix: String(rule?.prefix ?? "").trim().toLowerCase(),
        contextWindow: positiveInteger(rule?.contextWindow, 0),
      }))
      .filter((rule) => rule.prefix && rule.contextWindow >= 2_000)
      .sort((left, right) => right.prefix.length - left.prefix.length);
  } catch {
    return [];
  }
}

function cursorContextWindow(model, rules = CURSOR_MODEL_CONTEXT_RULES, fallback = CURSOR_DEFAULT_CONTEXT_WINDOW) {
  const modelId = String(model ?? "").trim().toLowerCase();
  return rules.find((rule) => modelId.includes(rule.prefix))?.contextWindow ?? fallback;
}

function contextPressureTier(currentTokens, contextWindow = CURSOR_DEFAULT_CONTEXT_WINDOW) {
  if (!(currentTokens > 0) || !(contextWindow > 0)) return "normal";
  const ratio = currentTokens / contextWindow;
  if (ratio >= 0.9) return "force";
  if (ratio >= 0.8) return "elide";
  if (ratio >= 0.6) return "snip";
  if (ratio >= 0.5) return "warn";
  return "normal";
}

function positiveInteger(value, fallback) {
  const parsed = Number.parseInt(value ?? "", 10);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : fallback;
}

function isRetryableCursorError(error) {
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

function shouldSilentRetryCursorTurn(error, { producedOutput = false, attempt = 0, maxRetries = CURSOR_SILENT_RETRIES } = {}) {
  // After UI output, the caller continues with "go on" plus interrupted pending-turn context
  // (same as a manual go-on) instead of replaying the original prompt.
  void producedOutput;
  return attempt < maxRetries
    && (error instanceof CursorStartupTimeout || isRetryableCursorError(error));
}

async function createCursorAgent(options, sdk = Agent, retryDelaysMs = CURSOR_CREATE_RETRY_DELAYS_MS) {
  for (let attempt = 0; ; attempt += 1) {
    try {
      return await sdk.create(options);
    } catch (error) {
      if (attempt >= retryDelaysMs.length || !isRetryableCursorError(error)) throw error;
      // Keep create retries silent: Nova surfaces stderr as bridge failures.
      await new Promise((resolve) => setTimeout(resolve, retryDelaysMs[attempt]));
    }
  }
}

function isNovaDenyTaskHook(entry) {
  return typeof entry?.command === "string" && entry.command.includes(NOVA_DENY_TASK_MARKER);
}

function novaDenyTaskHookCommand() {
  return `node ./hooks/${NOVA_DENY_TASK_SCRIPT}`;
}

function mergeNovaTaskDenyHooks(config = {}) {
  const hooks = { ...(config.hooks && typeof config.hooks === "object" ? config.hooks : {}) };
  const denyEntry = {
    command: novaDenyTaskHookCommand(),
    failClosed: true,
  };
  const taskEntry = { ...denyEntry, matcher: "Task" };
  const preToolUse = (Array.isArray(hooks.preToolUse) ? hooks.preToolUse : [])
    .filter((entry) => !isNovaDenyTaskHook(entry));
  preToolUse.push(taskEntry);
  hooks.preToolUse = preToolUse;
  const subagentStart = (Array.isArray(hooks.subagentStart) ? hooks.subagentStart : [])
    .filter((entry) => !isNovaDenyTaskHook(entry));
  subagentStart.push(denyEntry);
  hooks.subagentStart = subagentStart;
  return {
    version: Number.isFinite(config.version) ? config.version : 1,
    ...config,
    hooks,
  };
}

async function ensureGlobalTaskDenyHooks(cursorDir = CURSOR_USER_DIR) {
  const hooksDir = join(cursorDir, "hooks");
  const scriptPath = join(hooksDir, NOVA_DENY_TASK_SCRIPT);
  const hooksPath = join(cursorDir, "hooks.json");
  await mkdir(hooksDir, { recursive: true });
  await writeFile(scriptPath, NOVA_DENY_TASK_SCRIPT_SOURCE, "utf8");
  let existing = {};
  try {
    existing = JSON.parse(await readFile(hooksPath, "utf8"));
    if (!existing || typeof existing !== "object" || Array.isArray(existing)) existing = {};
  } catch (error) {
    if (error?.code !== "ENOENT") {
      process.stderr.write(`Cursor hooks.json unreadable; rewriting Nova Task deny: ${error instanceof Error ? error.message : String(error)}\n`);
      existing = {};
    }
  }
  const next = mergeNovaTaskDenyHooks(existing);
  await writeFile(hooksPath, `${JSON.stringify(next, null, 2)}\n`, "utf8");
  return { hooksPath, scriptPath, config: next };
}

class CursorStartupTimeout extends Error {
  constructor(phase) {
    super(`Cursor ${phase} timed out before producing output`);
    this.name = "CursorStartupTimeout";
  }
}

function withTimeout(promise, timeoutMs, phase) {
  if (!timeoutMs) return promise;
  let timer;
  return Promise.race([
    promise,
    new Promise((_, reject) => {
      timer = setTimeout(() => reject(new CursorStartupTimeout(phase)), timeoutMs);
    }),
  ]).finally(() => clearTimeout(timer));
}

function compactConversation(turns, maxChars = CURSOR_RECOVERY_CONTEXT_CHARS) {
  const entries = [];
  for (const conversation of turns ?? []) {
    if (conversation?.type !== "agentConversationTurn") continue;
    const user = conversation.turn?.userMessage?.text?.trim();
    if (user) entries.push(`User: ${user}`);
    const assistant = (conversation.turn?.steps ?? [])
      .filter((step) => step.type === "assistantMessage")
      .map((step) => step.message?.text?.trim())
      .filter(Boolean)
      .join("\n");
    if (assistant) entries.push(`Assistant: ${assistant}`);
  }
  const selected = [];
  let used = 0;
  for (let index = entries.length - 1; index >= 0; index -= 1) {
    const entry = entries[index];
    const remaining = maxChars - used;
    if (remaining <= 0) break;
    selected.unshift(entry.length <= remaining
      ? entry
      : remaining === 1 ? "…" : `…${entry.slice(-(remaining - 1))}`);
    used += Math.min(entry.length, remaining) + 2;
  }
  return selected.join("\n\n");
}

function createSlimMemory() {
  return {
    digests: [],
    preservedUserPrompts: [],
    turns: [],
    pendingTurn: "",
    contextTokens: 0,
    contextTier: "normal",
    rewriteVersion: 0,
    policyHash: "",
    toolSchemaHash: "",
    lastShapeRewriteVersion: 0,
    consecutiveCompactions: 0,
    compactStuck: false,
  };
}

function isSlimMemoryEmpty(memory) {
  return !(memory?.digests?.length || memory?.turns?.length || memory?.pendingTurn);
}

function messageText(message) {
  return typeof message === "string" ? message : (message?.text ?? "");
}

function withMessageText(message, text) {
  return typeof message === "string" ? text : { ...message, text };
}

function formatSlimMemory(memory) {
  const sections = [
    "Continue this conversation using the append-only transcript below.",
    "Prior full chat checkpoints and completed tool traces are intentionally omitted.",
    "Do not ask the user to repeat earlier requests; use the transcript and latest user message.",
    "",
    "## Conversation",
  ];
  if (memory.preservedUserPrompts?.length) {
    sections.push("", "### Preserved user requests");
    memory.preservedUserPrompts.forEach((prompt) => sections.push(`User:\n${prompt}`));
  }
  if (memory.digests?.length) {
    sections.push("", "### Frozen digests");
    memory.digests.forEach((digest, index) => sections.push(`Digest ${index + 1}:\n${digest}`));
  }
  for (const turn of memory.turns ?? []) {
    sections.push("", `User:\n${turn.userPrompt}`);
    if (turn.conclusion) sections.push(`Assistant:\n${turn.conclusion}`);
  }
  if (memory.pendingTurn) {
    // Replay the failed prompt at the exact location where it appeared previously, then append its
    // recovered trajectory and continuation hint. Do not insert a heading before it: that would
    // invalidate the otherwise reusable prefix after an interrupted turn.
    sections.push(
      "",
      memory.pendingTurn,
      "",
      "[Interrupted turn: complete working context above. Continue from its assistant and tool trace instead of starting over.]",
    );
  }
  return sections.join("\n");
}

/**
 * Format a turn trajectory for slim-memory. Completed turns omit thinking (same policy as Vega
 * super-context): only interrupted/native trajectories should replay exposed reasoning.
 */
function formatTurnTrace(userMessage, state, { includeThinking = false } = {}) {
  const sections = [];
  const prompt = String(messageText(userMessage)).trim();
  if (prompt) sections.push(`User:\n${prompt}`);
  for (const entry of state?.trace ?? []) {
    if (entry.kind === "thinking") {
      if (!includeThinking) continue;
      const text = String(entry.text ?? "").trim();
      if (text) sections.push(`Assistant reasoning:\n${text}`);
      continue;
    }
    if (entry.kind === "assistant") {
      const text = String(entry.text ?? "").trim();
      if (text) sections.push(`Assistant:\n${text}`);
      continue;
    }
    if (entry.kind !== "tool") continue;
    const tool = entry.item;
    const details = [`[Tool] ${tool.tool ?? "unknown"} (${tool.status ?? "in_progress"})`];
    if (tool.arguments !== undefined) details.push(`Arguments: ${safeJson(tool.arguments)}`);
    if (tool.result !== undefined) details.push(`Result: ${safeJson(tool.result)}`);
    sections.push(details.join("\n"));
  }
  return sections.join("\n\n");
}

function formatInterruptedTurn(userMessage, state) {
  return formatTurnTrace(userMessage, state, { includeThinking: true });
}

function pendingTurnContext(previousPending, userMessage, state) {
  const current = formatInterruptedTurn(userMessage, state);
  return [String(previousPending ?? "").trim(), current].filter(Boolean).join("\n\n");
}

function formatCompletedTurn(userMessage, state) {
  return formatTurnTrace(userMessage, state, { includeThinking: false });
}

function safeJson(value) {
  let text;
  if (typeof value === "string") text = value;
  else {
    try {
      text = JSON.stringify(value);
    } catch {
      text = String(value);
    }
  }
  if (text.length <= CURSOR_TRACE_TOOL_OUTPUT_MAX_CHARS) return text;
  const notice = `\n…[elided Cursor tool trace — ${text.length} chars; re-run the tool if needed]…\n`;
  const budget = CURSOR_TRACE_TOOL_OUTPUT_MAX_CHARS - notice.length;
  return `${text.slice(0, Math.ceil(budget * 0.6))}${notice}${text.slice(-Math.floor(budget * 0.4))}`;
}

function messageWithSlimMemory(message, memory) {
  // Always use the same envelope, including on the first turn. After a successful turn the next
  // prompt is the previous prompt plus only `Assistant` and `User` suffixes, so provider prefix
  // caches can reuse every byte before the newly appended content.
  return withMessageText(message, `${formatSlimMemory(memory)}\n\nUser:\n${messageText(message)}`);
}

/**
 * Attach Vega-style batch FS / search policy + concise reply style on every user turn.
 * Cursor has no durable systemPrompt, and Nova creates a fresh Agent per prompt,
 * so this cannot be "first message only".
 */
function messageWithToolPolicy(message, options = {}) {
  const prefix = cursorPromptPrefix(options);
  if (!prefix) return message;
  return withMessageText(message, `${prefix}\n\n${messageText(message)}`);
}

function messageWithRecoveryContext(message, history) {
  if (!history) return message;
  const prefix = [
    "Continue this recovered conversation. The following is a compact transcript of the most recent relevant context.",
    "Do not mention recovery or ask the user to repeat anything. Continue the current request normally.",
    "",
    history,
    "",
    "Current request:",
  ].join("\n");
  return withMessageText(message, `${prefix}\n${messageText(message)}`);
}

function contextTokensFromUsage(usage) {
  if (!usage || typeof usage !== "object") return 0;
  // Cursor input already includes cache reads; cache writes are reported inside output but also
  // become input context. Exclude uncached model output and count each cache category once.
  const input = Number(usage.inputTokens ?? usage.input_tokens) || 0;
  const cacheWrite = Number(usage.cacheWriteTokens ?? usage.cache_write_tokens) || 0;
  return input + cacheWrite;
}

function cursorUsageField(usage, camel, snake) {
  return Number(usage?.[camel] ?? usage?.[snake]) || 0;
}

/** Field-wise sum matching Cursor SDK `sumTokenUsage` (input/cache fields are disjoint). */
function mergeCursorUsage(total, usage) {
  if (!usage || typeof usage !== "object") return total;
  const merged = {
    inputTokens: cursorUsageField(total, "inputTokens", "input_tokens")
      + cursorUsageField(usage, "inputTokens", "input_tokens"),
    outputTokens: cursorUsageField(total, "outputTokens", "output_tokens")
      + cursorUsageField(usage, "outputTokens", "output_tokens"),
    cacheReadTokens: cursorUsageField(total, "cacheReadTokens", "cache_read_tokens")
      + cursorUsageField(usage, "cacheReadTokens", "cache_read_tokens"),
    cacheWriteTokens: cursorUsageField(total, "cacheWriteTokens", "cache_write_tokens")
      + cursorUsageField(usage, "cacheWriteTokens", "cache_write_tokens"),
  };
  merged.totalTokens = merged.inputTokens + merged.outputTokens
    + merged.cacheReadTokens + merged.cacheWriteTokens;
  const reasoning = cursorUsageField(total, "reasoningTokens", "reasoning_tokens")
    + cursorUsageField(usage, "reasoningTokens", "reasoning_tokens");
  if (reasoning > 0) merged.reasoningTokens = reasoning;
  return merged;
}

function cursorUsageTotal(usage) {
  if (!usage || typeof usage !== "object") return 0;
  const total = Number(usage.totalTokens ?? usage.total_tokens);
  if (total > 0) return total;
  return contextTokensFromUsage(usage) + cursorUsageField(usage, "outputTokens", "output_tokens");
}

function cursorRunUsage(result, accumulatedStreamUsage) {
  // Stream usage is emitted once per model turn, while wait() reports the cumulative
  // usage for the whole run. Prefer the run total for billing; fall back to a summed stream.
  const fromResult = result?.usage;
  if (cursorUsageTotal(fromResult) > 0) return fromResult;
  return accumulatedStreamUsage;
}

/** Cursor input includes cache reads and output includes cache writes; emit disjoint fields. */
function normalizeCursorUsageForNova(usage) {
  if (!usage || typeof usage !== "object") return usage;
  const input = cursorUsageField(usage, "inputTokens", "input_tokens");
  const output = cursorUsageField(usage, "outputTokens", "output_tokens");
  const cacheRead = cursorUsageField(usage, "cacheReadTokens", "cache_read_tokens");
  const cacheWrite = cursorUsageField(usage, "cacheWriteTokens", "cache_write_tokens");
  const uncachedInput = Math.max(0, input - cacheRead);
  const uncachedOutput = Math.max(0, output - cacheWrite);
  const normalized = {
    inputTokens: uncachedInput,
    outputTokens: uncachedOutput,
    cacheReadTokens: cacheRead,
    cacheWriteTokens: cacheWrite,
    totalTokens: uncachedInput + uncachedOutput + cacheRead + cacheWrite,
  };
  const reasoning = cursorUsageField(usage, "reasoningTokens", "reasoning_tokens");
  if (reasoning > 0) normalized.reasoningTokens = reasoning;
  return normalized;
}

function extractTurnConclusion(state, result) {
  const fromResult = String(result?.result ?? "").trim();
  if (fromResult) return fromResult;
  const assistantTexts = [...(state?.texts?.entries?.() ?? [])]
    .filter(([id]) => String(id).includes("-assistant-"))
    .map(([, text]) => String(text ?? "").trim())
    .filter(Boolean);
  return assistantTexts.at(-1) ?? "";
}

function recordSlimTurn(memory, userMessage, conclusion) {
  const userPrompt = String(messageText(userMessage)).trim();
  const turnConclusion = String(conclusion ?? "").trim();
  if (userPrompt || turnConclusion) memory.turns.push({ userPrompt, conclusion: turnConclusion });
  return memory;
}

async function compressSlimMemory(memory, summarize, {
  maxChars = Math.max(32_000, Math.floor(CURSOR_DEFAULT_CONTEXT_WINDOW * 0.75 * 4)),
  currentTokens = memory.contextTokens ?? 0,
  maxTokens = Math.max(2_000, Math.floor(CURSOR_DEFAULT_CONTEXT_WINDOW * 0.75)),
} = {}) {
  const formattedLength = formatSlimMemory({ ...memory, pendingTurn: "" }).length;
  const belowCapacity = currentTokens > 0
    ? currentTokens < maxTokens
    : formattedLength < maxChars;
  if (belowCapacity) {
    memory.consecutiveCompactions = 0;
    memory.compactStuck = false;
    return false;
  }
  if (memory.compactStuck || memory.turns.length < 2) return false;
  if ((memory.consecutiveCompactions ?? 0) >= 2) {
    // A tiny effective window can otherwise rewrite the prefix every turn forever. Freeze the
    // current shape and let the provider reject naturally rather than permanently defeating cache.
    memory.compactStuck = true;
    return false;
  }
  const latestTurn = memory.turns.at(-1);
  const compactedTurns = memory.turns.slice(0, -1);
  const preservedPrompts = compactedTurns
    .map((turn) => String(turn.userPrompt ?? "").trim())
    .filter((prompt) => prompt && prompt.length <= 2_000 && !memory.preservedUserPrompts.includes(prompt));
  const earlierTurns = { ...createSlimMemory(), turns: compactedTurns };
  const digest = String(await summarize(formatSlimMemory(earlierTurns)) ?? "").trim();
  if (!digest) return false;
  // Digests accumulate and are immutable. Never roll an old digest into a replacement summary.
  memory.preservedUserPrompts.push(...preservedPrompts);
  memory.digests.push(digest);
  memory.turns = [latestTurn];
  memory.rewriteVersion = (memory.rewriteVersion ?? 0) + 1;
  memory.consecutiveCompactions = (memory.consecutiveCompactions ?? 0) + 1;
  return true;
}

async function summarizeSlimMemory(memory, request, sdk = Agent, compressionOptions = {}) {
  return compressSlimMemory(memory, async (earlierTurns) => {
    const agent = await sdk.create({
      apiKey: process.env.CURSOR_API_KEY,
      model: modelSelection(request.model),
      local: { cwd: request.cwd },
    });
    try {
      const run = await agent.send([
        "Summarize the earlier conversation turns below for another coding agent.",
        "Preserve user intent, decisions, changed files, important identifiers, constraints, and unresolved work.",
        "Do not copy the transcript or add commentary. Do not omit facts merely to sound concise.",
        "",
        earlierTurns,
      ].join("\n"));
      const result = await run.wait();
      if (result.status === "error") throw new Error(result.error?.message || "Cursor memory summary failed");
      return result.result;
    } finally {
      agent.close();
    }
  }, compressionOptions);
}

function slimMemoryPath(sessionKey) {
  return join(CURSOR_SLIM_MEMORY_DIR, `${sessionKey}.json`);
}

function threadMemoryKey(threadId) {
  if (!threadId) return undefined;
  const identity = String(threadId);
  const safeIdentity = /^[A-Za-z0-9_-]+$/.test(identity)
    ? identity
    : createHash("sha256").update(identity).digest("hex");
  // Keep Nova thread ownership separate from Cursor provider session ids. Provider ids can
  // occasionally be reused or restored incorrectly; they must never select another thread's
  // compact memory file.
  return `nova-thread-${safeIdentity}`;
}

async function loadSlimMemory(sessionKey) {
  if (!sessionKey) return createSlimMemory();
  try {
    const parsed = JSON.parse(await readFile(slimMemoryPath(sessionKey), "utf8"));
    if (Array.isArray(parsed?.turns)) {
      const legacySummary = String(parsed.summary ?? "").trim();
      return {
        ...createSlimMemory(),
        digests: Array.isArray(parsed.digests)
          ? parsed.digests.map(String).filter(Boolean)
          : legacySummary ? [legacySummary] : [],
        preservedUserPrompts: Array.isArray(parsed.preservedUserPrompts)
          ? parsed.preservedUserPrompts.map(String).filter(Boolean)
          : [],
        turns: parsed.turns.map((turn) => ({
          userPrompt: String(turn?.userPrompt ?? ""),
          conclusion: String(turn?.conclusion ?? ""),
        })),
        pendingTurn: String(parsed.pendingTurn ?? ""),
        contextTokens: Number(parsed.contextTokens) || 0,
        contextTier: ["normal", "warn", "snip", "elide", "force"].includes(parsed.contextTier)
          ? parsed.contextTier
          : "normal",
        rewriteVersion: Number(parsed.rewriteVersion) || 0,
        policyHash: String(parsed.policyHash ?? ""),
        toolSchemaHash: String(parsed.toolSchemaHash ?? ""),
        lastShapeRewriteVersion: Number(parsed.lastShapeRewriteVersion) || 0,
        consecutiveCompactions: Number(parsed.consecutiveCompactions) || 0,
        compactStuck: parsed.compactStuck === true,
      };
    }
    // Migrate the first slim-memory format, which stored prompts and conclusions separately.
    const prompts = Array.isArray(parsed?.userPrompts) ? parsed.userPrompts : [];
    const conclusions = Array.isArray(parsed?.conclusions) ? parsed.conclusions : [];
    return {
      ...createSlimMemory(),
      turns: Array.from({ length: Math.max(prompts.length, conclusions.length) }, (_, index) => ({
        userPrompt: String(prompts[index] ?? ""),
        conclusion: String(conclusions[index] ?? ""),
      })),
    };
  } catch {
    return createSlimMemory();
  }
}

async function saveSlimMemory(sessionKey, memory) {
  if (!sessionKey) return;
  await mkdir(CURSOR_SLIM_MEMORY_DIR, { recursive: true });
  await writeFile(slimMemoryPath(sessionKey), `${JSON.stringify({
    version: 6,
    digests: memory.digests ?? [],
    preservedUserPrompts: memory.preservedUserPrompts ?? [],
    turns: memory.turns ?? [],
    pendingTurn: memory.pendingTurn ?? "",
    contextTokens: memory.contextTokens ?? 0,
    contextTier: memory.contextTier ?? "normal",
    rewriteVersion: memory.rewriteVersion ?? 0,
    policyHash: memory.policyHash ?? "",
    toolSchemaHash: memory.toolSchemaHash ?? "",
    lastShapeRewriteVersion: memory.lastShapeRewriteVersion ?? 0,
    consecutiveCompactions: memory.consecutiveCompactions ?? 0,
    compactStuck: memory.compactStuck === true,
  })}\n`, "utf8");
}

function ingestCompactHistory(memory, history) {
  for (const block of String(history ?? "").split(/\n\n+/)) {
    if (block.startsWith("User: ")) {
      memory.turns.push({ userPrompt: block.slice(6).trim(), conclusion: "" });
    } else if (block.startsWith("Assistant: ")) {
      const conclusion = block.slice(11).trim();
      const turn = memory.turns.at(-1);
      if (turn && !turn.conclusion) turn.conclusion = conclusion;
      else if (conclusion) memory.turns.push({ userPrompt: "", conclusion });
    }
  }
  return memory;
}

async function seedSlimMemoryFromSession(memory, sessionId, request, sdk = Agent) {
  if (!sessionId || !isSlimMemoryEmpty(memory)) return false;
  try {
    // Read completed run transcripts directly. Resuming the checkpoint only to inspect history is
    // both slow and unnecessary, and can restore poisoned executor state.
    const runs = await withTimeout(
      sdk.listRuns(sessionId, { runtime: "local", cwd: request.cwd }),
      CURSOR_RECOVERY_TIMEOUT_MS,
      "list runs",
    ).catch(() => ({ items: [] }));
    const history = await recoveryHistory(runs.items);
    ingestCompactHistory(memory, history);
    return !isSlimMemoryEmpty(memory);
  } catch {
    return false;
  }
}

async function recoveryHistory(runs, timeoutMs = CURSOR_RECOVERY_TIMEOUT_MS) {
  const candidates = [...(runs ?? [])]
    .filter((run) => ["completed", "finished"].includes(String(run.status).toLowerCase()))
    .sort((left, right) => (right.createdAt ?? 0) - (left.createdAt ?? 0));
  for (const run of candidates) {
    try {
      const conversation = await withTimeout(run.conversation(), timeoutMs, "conversation");
      const history = compactConversation(conversation);
      if (history) return history;
    } catch {
      // A detached local run may not expose its transcript; try the previous run.
    }
  }
  return "";
}

async function recoverTimedOutAgent(
  agent,
  activeRun,
  request,
  sdk = Agent,
  timeoutMs = CURSOR_RECOVERY_TIMEOUT_MS,
) {
  const agentId = agent.agentId;
  await withTimeout(Promise.resolve(activeRun?.cancel?.()), timeoutMs, "cancel").catch(() => {});
  const runs = await withTimeout(
    sdk.listRuns(agentId, { runtime: "local", cwd: request.cwd }),
    timeoutMs,
    "list runs",
  ).catch(() => ({ items: [] }));
  await Promise.all((runs.items ?? [])
    .filter((run) => !TERMINAL_RUN_STATUSES.has(String(run.status).toLowerCase()))
    .map((run) => withTimeout(
      sdk.cancelRun(run.id, { runtime: "local", cwd: request.cwd }),
      timeoutMs,
      "cancel run",
    ).catch(() => {})));
  agent.close();
  const options = {
    apiKey: process.env.CURSOR_API_KEY,
    model: modelSelection(request.model),
    local: {
      cwd: request.cwd,
      customTools: createCursorFilesystemTools(request.cwd, { readOnly: request.mode === "plan" }),
    },
  };
  return {
    agent: await withTimeout(createCursorAgent(options, sdk), timeoutMs, "create"),
    history: "",
    replaced: true,
  };
}

function sendTiming(phase, startedAt, details = {}) {
  send({ type: "timing", phase, elapsedMs: Math.round(performance.now() - startedAt), ...details });
}

function isActiveRunError(error) {
  return String(error).includes("already has active run");
}

async function sendPromptWithRecovery(
  agent,
  request,
  message,
  options,
  sdk = Agent,
  emitTiming = sendTiming,
  bootstrapMessage = message,
) {
  const sendStartedAt = performance.now();
  try {
    const run = await agent.send(message, options);
    emitTiming("send", sendStartedAt);
    return { agent, run };
  } catch (error) {
    if (!isActiveRunError(error)) throw error;
    emitTiming("send_active_run", sendStartedAt);
  }

  const cleanupStartedAt = performance.now();
  const runs = await sdk.listRuns(agent.agentId, { runtime: "local", cwd: request.cwd });
  const activeRuns = runs.items.filter((run) => !TERMINAL_RUN_STATUSES.has(String(run.status).toLowerCase()));
  for (const run of activeRuns) {
    await sdk.cancelRun(run.id, { runtime: "local", cwd: request.cwd });
  }
  emitTiming("active_run_cleanup", cleanupStartedAt, { cancelledRuns: activeRuns.length });

  const retryStartedAt = performance.now();
  try {
    const run = await agent.send(message, options);
    emitTiming("send_retry", retryStartedAt);
    return { agent, run };
  } catch (error) {
    if (!isActiveRunError(error)) throw error;
    emitTiming("send_retry_active_run", retryStartedAt);
  }

  // The live session is still wedged after cancelling its active runs. Do not pay checkpoint-resume
  // latency or inherit poisoned runtime state: recreate once and bootstrap from Vega's transcript.
  const recreateStartedAt = performance.now();
  agent.close();
  const freshAgent = await createCursorAgent(agentCreateOptions(request), sdk);
  emitTiming("agent_recreate", recreateStartedAt);
  const finalSendStartedAt = performance.now();
  const run = await freshAgent.send(bootstrapMessage, options);
  emitTiming("send_after_recreate", finalSendStartedAt);
  return { agent: freshAgent, run, replaced: true };
}

function agentFingerprint(request) {
  const model = String(request?.model ?? "");
  const cwd = String(request?.cwd ?? "");
  const mode = request?.mode === "plan" ? "plan" : "agent";
  return `${model}\0${cwd}\0${mode}`;
}

function canReuseAgentSession(agent, session, request, sessionKey) {
  return Boolean(agent
    && session
    && session.sessionKey === sessionKey
    && session.fingerprint === agentFingerprint(request));
}

function cursorToolShape(request) {
  const tools = createCursorFilesystemTools(request?.cwd, { readOnly: request?.mode === "plan" });
  return Object.entries(tools).map(([name, tool]) => ({
    name,
    description: tool.description ?? "",
    inputSchema: tool.inputSchema ?? null,
  }));
}

function agentCreateOptions(request) {
  const readOnly = request?.mode === "plan";
  return {
    apiKey: process.env.CURSOR_API_KEY,
    model: modelSelection(request?.model),
    local: {
      cwd: request?.cwd,
      // Vega-style batch FS tools; inlined via SDK (unlike hooks, which are file-only).
      customTools: createCursorFilesystemTools(request?.cwd, { readOnly }),
    },
  };
}

function prewarmEnabled(env = process.env) {
  return env.NOVA_CURSOR_PREWARM !== "0";
}

// Single-slot idle Agent prewarm: keep one unused Agent.create() ready for the last
// model/cwd/mode fingerprint. Consume → refill immediately; matching in-flight creates
// are awaited instead of starting a parallel cold create.
function createAgentPrewarm(sdk = Agent, { enabled = prewarmEnabled() } = {}) {
  let slot = null;

  function discard() {
    const current = slot;
    slot = null;
    if (current?.agent) {
      try { current.agent.close(); } catch { /* already closed */ }
    }
  }

  function ensure(fingerprint, options) {
    if (!enabled) return;
    if (slot?.fingerprint === fingerprint && (slot.agent || slot.promise)) return;
    discard();
    const entry = { fingerprint, agent: null, promise: null };
    entry.promise = (async () => {
      try {
        const agent = await createCursorAgent(options, sdk);
        if (slot !== entry) {
          try { agent.close(); } catch { /* discarded */ }
          return null;
        }
        entry.agent = agent;
        entry.promise = null;
        return agent;
      } catch (error) {
        if (slot === entry) slot = null;
        process.stderr.write(`Cursor agent prewarm failed: ${error instanceof Error ? error.message : String(error)}\n`);
        return null;
      }
    })();
    slot = entry;
  }

  async function acquire(request) {
    const fingerprint = agentFingerprint(request);
    const options = agentCreateOptions(request);

    const take = (agent, source) => {
      if (slot?.fingerprint === fingerprint) slot = null;
      ensure(fingerprint, options);
      return { agent, source, fingerprint };
    };

    if (enabled && slot?.fingerprint === fingerprint) {
      if (slot.agent) return take(slot.agent, "prewarm");
      if (slot.promise) {
        const agent = await slot.promise;
        if (agent) return take(agent, "prewarm_wait");
      }
    } else if (slot && slot.fingerprint !== fingerprint) {
      discard();
    }

    const agent = await createCursorAgent(options, sdk);
    ensure(fingerprint, options);
    return { agent, source: "live", fingerprint };
  }

  return {
    acquire,
    ensure,
    discard,
    close: discard,
    get snapshot() {
      return slot
        ? {
          fingerprint: slot.fingerprint,
          ready: Boolean(slot.agent),
          pending: Boolean(slot.promise),
        }
        : null;
    },
  };
}

async function generateTitle(request) {
  const agent = await createCursorAgent({
    apiKey: process.env.CURSOR_API_KEY,
    model: modelSelection(request.model),
    local: { cwd: request.cwd },
  });
  try {
    const run = await agent.send(request.prompt);
    const result = await run.wait();
    if (result.status === "error") throw new Error(result.error?.message || "Cursor title generation failed");
    if (result.result) return result.result;
    const turns = await run.conversation();
    return turns
      .flatMap((turn) => turn.type === "agentConversationTurn" ? turn.turn.steps : [])
      .filter((step) => step.type === "assistantMessage")
      .map((step) => step.message.text)
      .at(-1) ?? "";
  } finally {
    agent.close();
  }
}

async function promptMessage(parts) {
  const textParts = parts.filter((part) => part.type === "text").map((part) => part.text);
  const images = [];
  const mediaTypes = { ".jpg": "image/jpeg", ".jpeg": "image/jpeg", ".png": "image/png", ".gif": "image/gif", ".webp": "image/webp" };
  for (const part of parts) {
    if (part.type === "image_data") images.push({ data: part.data, mimeType: part.mime });
    if (part.type === "local_image") {
      const mimeType = mediaTypes[extname(part.path).toLowerCase()];
      if (mimeType) images.push({ data: (await readFile(part.path)).toString("base64"), mimeType });
      else textParts.push(`Attached file: ${part.path}`);
    }
  }
  const text = textParts.join("\n\n");
  return images.length ? { text, images } : text;
}

async function main() {
  // Cursor has no built-in Task toggle; install user-global deny hooks before any Agent.create.
  await ensureGlobalTaskDenyHooks().catch((error) => {
    process.stderr.write(`Cursor global Task-deny hooks failed: ${error instanceof Error ? error.message : String(error)}\n`);
  });
  const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
  const requests = [];
  let wake;
  let activeRun;
  let preserveActiveTurn;
  let closed = false;
  lines.on("line", (line) => {
    const request = JSON.parse(line);
    if (request.action === "cancel") {
      // Update bridge memory immediately, but keep persistence and SDK cancellation off the user
      // path. Rust also injects its streamed transcript into the replacement prompt, so this write
      // is crash recovery rather than a handoff barrier.
      void Promise.resolve(preserveActiveTurn?.()).catch((error) => {
        process.stderr.write(`Cursor pending-turn persistence failed: ${error instanceof Error ? error.message : String(error)}\n`);
      });
      void Promise.resolve(activeRun?.cancel()).catch(() => {});
      return;
    }
    requests.push(request);
    wake?.();
    wake = undefined;
  });
  lines.on("close", () => {
    closed = true;
    wake?.();
  });
  let agent;
  let agentSession;
  let sessionKey;
  let memoryKey;
  let memory = createSlimMemory();
  const prewarm = createAgentPrewarm(Agent);
  while (!closed || requests.length) {
    if (!requests.length) await new Promise((resolve) => { wake = resolve; });
    const request = requests.shift();
    if (!request) continue;
    try {
      if (request.action === "models") {
        send({ ok: true, data: await modelOptions() });
        continue;
      }
      if (request.action === "title") {
        send({ ok: true, data: await generateTitle(request) });
        continue;
      }
      if (request.action !== "prompt") throw new Error(`Unknown action: ${request.action}`);

      // The kept-alive bridge is owned by one Vega thread, so retain its live Cursor Agent between
      // completed turns. A changed thread/model/cwd/mode starts a new session and bootstraps it from
      // Vega's canonical slim memory; normal matching turns send only the new user message.
      const ownedThreadKey = threadMemoryKey(request.threadId);
      const nextSessionKey = ownedThreadKey || request.sessionId || sessionKey || randomUUID();
      const nextMemoryKey = ownedThreadKey || nextSessionKey;
      if (nextSessionKey !== sessionKey || nextMemoryKey !== memoryKey) {
        if (memoryKey) await saveSlimMemory(memoryKey, memory);
        sessionKey = nextSessionKey;
        memoryKey = nextMemoryKey;
        memory = await loadSlimMemory(memoryKey);
        // Legacy provider-session memory is deliberately not loaded here: if Cursor reused or
        // mis-restored that id, importing it would reproduce the exact cross-thread leak this
        // ownership key prevents. SDK history is only a best-effort bootstrap for an empty key.
        if (isSlimMemoryEmpty(memory) && request.sessionId) {
          await seedSlimMemoryFromSession(memory, request.sessionId, request);
          if (!isSlimMemoryEmpty(memory)) await saveSlimMemory(memoryKey, memory);
        }
      }

      const readOnly = request.mode === "plan";
      const originalMessage = await promptMessage(request.parts);
      const contextWindow = cursorContextWindow(request.model);
      const contextThreshold = Math.max(2_000, Math.floor(contextWindow * 0.75));
      const contextForceThreshold = Math.max(2_000, Math.floor(contextWindow * 0.9));
      const contextCharThreshold = Math.max(32_000, Math.floor(contextWindow * 0.75 * 4));
      const reuseSession = canReuseAgentSession(agent, agentSession, request, sessionKey);
      // A newly created/recovered Agent receives Vega's complete compact transcript once. A live
      // matching Agent already owns that history, so replaying it would duplicate every old turn.
      const bootstrapMessage = messageWithToolPolicy(
        messageWithSlimMemory(originalMessage, memory),
        { readOnly },
      );
      let message = reuseSession
        ? messageWithToolPolicy(originalMessage, { readOnly })
        : bootstrapMessage;
      // Build the SDK prompt before marking this turn pending, otherwise it would replay itself.
      // Persist first so even Agent.create/SDK initialization failures retain the user's request.
      const previousPendingTurn = memory.pendingTurn;
      memory.pendingTurn = pendingTurnContext(previousPendingTurn, originalMessage);
      await saveSlimMemory(memoryKey, memory).catch((error) => {
        process.stderr.write(`Cursor pending-turn persistence failed: ${error instanceof Error ? error.message : String(error)}\n`);
      });
      const acquireStartedAt = performance.now();
      if (reuseSession) {
        sendTiming("agent_acquire", acquireStartedAt, { source: "session" });
      } else {
        if (agent) {
          agent.close();
          agent = undefined;
          agentSession = undefined;
        }
        // Prefer the idle prewarm Agent.create only when a live session cannot be reused.
        const acquired = await prewarm.acquire(request);
        agent = acquired.agent;
        agentSession = { sessionKey, fingerprint: acquired.fingerprint };
        sendTiming("agent_acquire", acquireStartedAt, { source: acquired.source });
      }
      send({ type: "ready", sessionId: sessionKey });
      let completed = false;
      for (let attempt = 0; attempt <= CURSOR_SILENT_RETRIES && !completed; attempt += 1) {
        const state = createMessageState();
        preserveActiveTurn = () => {
          const pendingTurn = pendingTurnContext(previousPendingTurn, originalMessage, state);
          if (!pendingTurn) return undefined;
          memory.pendingTurn = pendingTurn;
          return saveSlimMemory(memoryKey, memory);
        };
        const turnStartedAt = performance.now();
        let attemptActive = true;
        let producedOutput = false;
        let resolveFirstActivity;
        const firstActivity = new Promise((resolve) => { resolveFirstActivity = resolve; });
        const markActivity = () => {
          if (producedOutput) return;
          producedOutput = true;
          resolveFirstActivity();
          sendTiming("first_delta", turnStartedAt);
        };
        const sendOptions = {
          mode: request.mode === "plan" ? "plan" : "agent",
          onDelta: ({ update }) => {
            if (!attemptActive) return;
            try {
              const item = mapDelta(update, state, activeRun?.id ?? "run");
              const plan = cursorTodoPlan(update.toolCall);
              if (item || plan) markActivity();
              if (item) send({ type: "item", item });
              if (plan) send({ type: "plan", plan });
            } catch (error) {
              process.stderr.write(`Cursor onDelta failed: ${error instanceof Error ? error.stack ?? error.message : String(error)}\n`);
            }
          },
        };
        try {
          const promptResult = await withTimeout(
            sendPromptWithRecovery(
              agent,
              request,
              message,
              sendOptions,
              Agent,
              sendTiming,
              bootstrapMessage,
            ),
            CURSOR_STARTUP_TIMEOUT_MS,
            "send",
          );
          agent = promptResult.agent;
          activeRun = promptResult.run;
          let lastStreamUsage;
          let accumulatedStreamUsage;
          const streamStartedAt = performance.now();
          const streamTask = (async () => {
            for await (const streamMessage of activeRun.stream()) {
              if (!attemptActive) continue;
              const items = mapMessage(streamMessage, state);
              const plan = streamMessage.type === "tool_call"
                ? cursorTodoPlan({ type: streamMessage.name, args: streamMessage.args, result: streamMessage.result })
                : null;
              if (items.length || plan) markActivity();
              for (const item of items) send({ type: "item", item });
              if (plan) send({ type: "plan", plan });
              if (streamMessage.type === "usage") {
                lastStreamUsage = streamMessage.usage;
                accumulatedStreamUsage = mergeCursorUsage(accumulatedStreamUsage, streamMessage.usage);
              }
            }
          })();
          // Cursor occasionally leaves a local run pending forever without yielding even one
          // event. Only that side-effect-free startup window is retried automatically.
          await withTimeout(
            Promise.race([firstActivity, streamTask]),
            CURSOR_STARTUP_TIMEOUT_MS,
            "stream startup",
          );
          await streamTask;
          sendTiming("stream", streamStartedAt);
          const waitStartedAt = performance.now();
          const waitTask = activeRun.wait();
          const result = producedOutput
            ? await waitTask
            : await withTimeout(waitTask, CURSOR_STARTUP_TIMEOUT_MS, "wait startup");
          sendTiming("wait", waitStartedAt);
          for (const item of completePendingTools(state)) send({ type: "item", item });
          if (result.status === "error") throw new Error(result.error?.message || "Cursor turn failed");
          memory.pendingTurn = "";
          recordSlimTurn(memory, originalMessage, extractTurnConclusion(state, result));
          // Billing = whole-run cumulative usage. Occupancy = latest model turn only;
          // summing every tool-loop input would far exceed the live context window.
          const turnUsage = cursorRunUsage(result, accumulatedStreamUsage);
          const occupancyUsage = lastStreamUsage ?? turnUsage;
          memory.contextTokens = contextTokensFromUsage(occupancyUsage);
          memory.contextTier = contextPressureTier(memory.contextTokens, contextWindow);
          const inputTokens = Number(occupancyUsage?.inputTokens ?? occupancyUsage?.input_tokens) || 0;
          const cacheReadTokens = Number(occupancyUsage?.cacheReadTokens ?? occupancyUsage?.cache_read_tokens) || 0;
          const cacheWriteTokens = Number(occupancyUsage?.cacheWriteTokens ?? occupancyUsage?.cache_write_tokens) || 0;
          const cacheDenominator = inputTokens >= cacheReadTokens + cacheWriteTokens
            ? inputTokens
            : inputTokens + cacheReadTokens + cacheWriteTokens;
          const policyHash = stableHash(cursorPromptPrefix({ readOnly }));
          const toolSchemaHash = stableHash(cursorToolShape(request));
          const cacheMissReasons = [];
          if (memory.policyHash && memory.policyHash !== policyHash) cacheMissReasons.push("system_changed");
          if (memory.toolSchemaHash && memory.toolSchemaHash !== toolSchemaHash) cacheMissReasons.push("tools_changed");
          if ((memory.lastShapeRewriteVersion ?? 0) !== (memory.rewriteVersion ?? 0)) cacheMissReasons.push("history_rewritten");
          memory.policyHash = policyHash;
          memory.toolSchemaHash = toolSchemaHash;
          memory.lastShapeRewriteVersion = memory.rewriteVersion ?? 0;
          send({
            type: "timing",
            phase: "context_shape",
            elapsedMs: 0,
            contextTier: memory.contextTier,
            contextWindow,
            rewriteVersion: memory.rewriteVersion ?? 0,
            cacheMissReasons,
            policyHash,
            toolSchemaHash,
            historyHash: stableHash(formatSlimMemory(memory)),
            inputTokens,
            cacheReadTokens,
            cacheWriteTokens,
            cacheHitRate: cacheDenominator > 0 ? cacheReadTokens / cacheDenominator : 0,
          });
          let compacted = false;
          if (["elide", "force"].includes(memory.contextTier)) {
            compacted = await summarizeSlimMemory(memory, request, Agent, {
              maxChars: contextCharThreshold,
              currentTokens: memory.contextTokens,
              maxTokens: contextThreshold,
            }).catch((error) => {
              process.stderr.write(`Cursor slim-memory compression failed: ${error instanceof Error ? error.message : String(error)}\n`);
              return false;
            });
            if (compacted) memory.contextTokens = 0;
          }
          // Cursor's live Agent owns native history. A compacted canonical epoch only takes effect
          // after rotating that Agent; at 90% rotate even if summarization failed to avoid overflow.
          if (compacted || memory.contextTier === "force" || memory.contextTokens >= contextForceThreshold) {
            agent.close();
            agent = undefined;
            agentSession = undefined;
            send({
              type: "timing",
              phase: "context_epoch_rotated",
              elapsedMs: 0,
              reason: compacted ? "compacted" : "force_threshold",
              rewriteVersion: memory.rewriteVersion ?? 0,
            });
          }
          await saveSlimMemory(memoryKey, memory).catch((error) => {
            process.stderr.write(`Cursor slim-memory persistence failed: ${error instanceof Error ? error.message : String(error)}\n`);
          });
          send({ type: "done", usage: normalizeCursorUsageForNova(turnUsage) });
          completed = true;
        } catch (error) {
          const retryable = shouldSilentRetryCursorTurn(error, { producedOutput, attempt });
          if (!retryable) {
            await preserveActiveTurn?.();
            throw error;
          }
          attemptActive = false;
          const continueAfterOutput = producedOutput;
          const retryDelayMs = CURSOR_SILENT_RETRY_DELAYS_MS[attempt]
            ?? CURSOR_SILENT_RETRY_DELAYS_MS.at(-1);
          // Match a manual "go on": persist the interrupted trajectory, then continue from it.
          if (continueAfterOutput) {
            await preserveActiveTurn?.();
            message = messageWithToolPolicy(
              messageWithSlimMemory("go on", memory),
              { readOnly },
            );
          }
          sendTiming("silent_retry", turnStartedAt, {
            attempt: attempt + 1,
            delayMs: retryDelayMs,
            reason: error instanceof CursorStartupTimeout ? "startup_timeout" : "retryable_error",
            continueWith: continueAfterOutput ? "go_on" : "same_prompt",
          });
          if (retryDelayMs > 0) {
            await new Promise((resolve) => setTimeout(resolve, retryDelayMs));
          }
          const recovery = await recoverTimedOutAgent(
            agent,
            activeRun,
            request,
            Agent,
            CURSOR_RECOVERY_TIMEOUT_MS,
          );
          agent = recovery.agent;
          agentSession = { sessionKey, fingerprint: agentFingerprint(request) };
          // Fresh Agent with no prior UI output bootstraps the full transcript. After UI output,
          // keep the "go on" prompt built above (interrupted pending-turn already in slim-memory).
          if (recovery.replaced && !continueAfterOutput) message = bootstrapMessage;
          // Keep the stable slim-memory session key; do not promote ephemeral agent ids.
          send({ type: "ready", sessionId: sessionKey });
          activeRun = undefined;
        } finally {
          attemptActive = false;
          preserveActiveTurn = undefined;
        }
      }
    } catch (error) {
      // Never carry a possibly poisoned live Agent across a failed prompt. Configuration/model-list
      // failures do not affect the retained conversation session.
      if (request.action === "prompt" && agent) {
        agent.close();
        agent = undefined;
        agentSession = undefined;
      }
      send({ ok: false, error: error instanceof Error ? error.message : String(error) });
    } finally {
      activeRun = undefined;
    }
  }
  agent?.close();
  prewarm.close();
}

export { main as runContextBridge };

export {
  CursorStartupTimeout,
  compactConversation,
  completePendingTools,
  compressSlimMemory,
  createMessageState,
  createSlimMemory,
  cursorModelOptions,
  cursorShellProgram,
  cursorTodoPlan,
  contextTokensFromUsage,
  cursorRunUsage,
  mergeCursorUsage,
  normalizeCursorUsageForNova,
  createCursorAgent,
  ensureGlobalTaskDenyHooks,
  extractTurnConclusion,
  formatCompletedTurn,
  formatInterruptedTurn,
  formatSlimMemory,
  pendingTurnContext,
  ingestCompactHistory,
  isEditFilesTool,
  isNovaDenyTaskHook,
  isRetryableCursorError,
  shouldSilentRetryCursorTurn,
  isSlimMemoryEmpty,
  mapDelta,
  mapMessage,
  mergeNovaTaskDenyHooks,
  messageWithRecoveryContext,
  messageWithSlimMemory,
  messageWithToolPolicy,
  modelSelection,
  novaDenyTaskHookCommand,
  promptMessage,
  recordSlimTurn,
  recoverTimedOutAgent,
  recoveryHistory,
  seedSlimMemoryFromSession,
  sendPromptWithRecovery,
  summarizeSlimMemory,
  threadMemoryKey,
  withTimeout,
  agentFingerprint,
  canReuseAgentSession,
  agentCreateOptions,
  contextPressureTier,
  cursorContextWindow,
  parseCursorModelContextRules,
  cursorToolShape,
  stableHash,
  createAgentPrewarm,
  prewarmEnabled,
};

export {
  createCursorFilesystemTools,
  cursorBatchToolPolicy,
  cursorCavemanPolicy,
  cursorPromptPrefix,
} from "./cursor-filesystem-tools.mjs";
