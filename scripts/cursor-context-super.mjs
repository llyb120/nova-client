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
const CURSOR_CREATE_RETRY_DELAYS_MS = [1_000, 3_000, 7_000];
const CURSOR_RECOVERY_CONTEXT_CHARS = positiveInteger(process.env.NOVA_CURSOR_RECOVERY_CONTEXT_CHARS, 24_000);
const CURSOR_SLIM_MEMORY_TURNS = positiveInteger(process.env.NOVA_CURSOR_SLIM_MEMORY_TURNS, 10);
const CURSOR_DEFAULT_CONTEXT_WINDOW = positiveInteger(process.env.NOVA_CURSOR_CONTEXT_WINDOW, 128_000);
const CURSOR_MODEL_CONTEXT_RULES = parseCursorModelContextRules(process.env.NOVA_CURSOR_MODEL_CONTEXTS);
const CURSOR_CONTEXT_THRESHOLD = Math.max(2_000, Math.floor(CURSOR_DEFAULT_CONTEXT_WINDOW * 0.75));
const CURSOR_CONTEXT_CHAR_THRESHOLD = Math.max(8_000, Math.floor(CURSOR_DEFAULT_CONTEXT_WINDOW * 0.75));
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

function positiveInteger(value, fallback) {
  const parsed = Number.parseInt(value ?? "", 10);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : fallback;
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

function contextThresholdsForModel(model) {
  const contextWindow = cursorContextWindow(model);
  return {
    contextWindow,
    maxTokens: Math.max(2_000, Math.floor(contextWindow * 0.75)),
    maxChars: Math.max(8_000, Math.floor(contextWindow * 0.75)),
  };
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
  return /API key exchange endpoint|fetch failed|ECONNRESET|ECONNREFUSED|ECONNABORTED|ETIMEDOUT|ENETUNREACH|EAI_AGAIN|socket hang up|other side closed|premature close|network connection lost|\b429\b|\b5\d\d\b/i
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
    summary: "",
    turns: [],
    pendingTurn: "",
    fullTurns: [],
    contextTokens: 0,
    contextStage: "full",
  };
}

function isSlimMemoryEmpty(memory) {
  return !(memory?.summary || memory?.turns?.length || memory?.pendingTurn || memory?.fullTurns?.length);
}

function messageText(message) {
  return typeof message === "string" ? message : (message?.text ?? "");
}

function withMessageText(message, text) {
  return typeof message === "string" ? text : { ...message, text };
}

function formatSlimMemory(memory) {
  const sections = [];
  if (memory.contextStage !== "slim" && memory.fullTurns?.length) {
    sections.push("## Complete earlier turns");
    memory.fullTurns.forEach((turn, index) => sections.push(`### Turn ${index + 1}`, turn));
  } else {
    if (memory.summary) sections.push("## Summary of earlier turns", memory.summary);
    if (memory.turns?.length) {
      if (sections.length) sections.push("");
      sections.push("## Recent turns");
      memory.turns.forEach((turn, index) => {
        sections.push(`### Turn ${index + 1}`, `User: ${turn.userPrompt}`);
        if (turn.conclusion) sections.push(`Conclusion: ${turn.conclusion}`);
      });
    }
  }
  if (memory.pendingTurn) {
    if (sections.length) sections.push("");
    sections.push(
      "## Interrupted turn (complete working context)",
      "This turn did not produce a conclusion. Continue from its assistant and tool trace instead of starting over.",
      memory.pendingTurn,
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
  if (typeof value === "string") return value;
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

function messageWithSlimMemory(message, memory) {
  if (isSlimMemoryEmpty(memory)) return message;
  const prefix = [
    "Continue this conversation using only the compact memory below.",
    "Prior tool traces and full chat checkpoints are intentionally omitted.",
    "Do not ask the user to repeat earlier requests; use the memory and current request.",
    "",
    formatSlimMemory(memory),
    "",
    "Current request:",
  ].join("\n");
  return withMessageText(message, `${prefix}\n${messageText(message)}`);
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
  const input = Number(usage.inputTokens ?? usage.input_tokens) || 0;
  const cacheWrite = Number(usage.cacheWriteTokens ?? usage.cache_write_tokens) || 0;
  return input + cacheWrite;
}

function cursorUsageField(usage, camel, snake) {
  return Number(usage?.[camel] ?? usage?.[snake]) || 0;
}

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
  const fromResult = result?.usage;
  return cursorUsageTotal(fromResult) > 0 ? fromResult : accumulatedStreamUsage;
}

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
  maxChars = CURSOR_CONTEXT_CHAR_THRESHOLD,
  currentTokens = memory.contextTokens ?? 0,
  maxTokens = CURSOR_CONTEXT_THRESHOLD,
} = {}) {
  const formattedLength = formatSlimMemory({ ...memory, pendingTurn: "", contextStage: "slim" }).length;
  const belowCapacity = currentTokens > 0
    ? currentTokens < maxTokens
    : formattedLength < maxChars;
  if (memory.contextStage !== "slim" || belowCapacity || memory.turns.length < 2) return false;
  const latestTurn = memory.turns.at(-1);
  const earlier = {
    summary: memory.summary,
    turns: memory.turns.slice(0, -1),
  };
  const summary = String(await summarize(formatSlimMemory(earlier)) ?? "").trim();
  if (!summary) return false;
  memory.summary = summary;
  // Compression is turn-based. The latest user prompt is always retained verbatim.
  memory.turns = [latestTurn];
  return true;
}

async function summarizeSlimMemory(memory, request, sdk = Agent) {
  const { maxTokens, maxChars } = contextThresholdsForModel(request?.model);
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
  }, {
    maxChars,
    maxTokens,
    currentTokens: memory.contextTokens ?? 0,
  });
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
      return {
        summary: String(parsed.summary ?? ""),
        turns: parsed.turns.map((turn) => ({
          userPrompt: String(turn?.userPrompt ?? ""),
          conclusion: String(turn?.conclusion ?? ""),
        })),
        pendingTurn: String(parsed.pendingTurn ?? ""),
        fullTurns: Array.isArray(parsed.fullTurns) ? parsed.fullTurns.map(String) : [],
        contextTokens: Number(parsed.contextTokens) || 0,
        // Older files only contain prompt/conclusion memory and must resume directly in stage two.
        contextStage: parsed.contextStage === "full" ? "full" : "slim",
      };
    }
    // Migrate the first slim-memory format, which stored prompts and conclusions separately.
    const prompts = Array.isArray(parsed?.userPrompts) ? parsed.userPrompts : [];
    const conclusions = Array.isArray(parsed?.conclusions) ? parsed.conclusions : [];
    return {
      ...createSlimMemory(),
      contextStage: "slim",
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
    version: 4,
    summary: memory.summary ?? "",
    turns: memory.turns ?? [],
    pendingTurn: memory.pendingTurn ?? "",
    fullTurns: memory.fullTurns ?? [],
    contextTokens: memory.contextTokens ?? 0,
    contextStage: memory.contextStage ?? "full",
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
  const options = {
    apiKey: process.env.CURSOR_API_KEY,
    model: modelSelection(request.model),
    local: { cwd: request.cwd },
  };
  let agent;
  try {
    agent = await withTimeout(sdk.resume(sessionId, options), CURSOR_RECOVERY_TIMEOUT_MS, "resume");
    const runs = await withTimeout(
      sdk.listRuns(sessionId, { runtime: "local", cwd: request.cwd }),
      CURSOR_RECOVERY_TIMEOUT_MS,
      "list runs",
    ).catch(() => ({ items: [] }));
    const history = await recoveryHistory(runs.items);
    ingestCompactHistory(memory, history);
    if (!isSlimMemoryEmpty(memory)) memory.contextStage = "slim";
    return !isSlimMemoryEmpty(memory);
  } catch {
    return false;
  } finally {
    agent?.close?.();
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
  createFresh = true,
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
  // Slim-memory mode prefers a fresh agent so poisoned checkpoints are never resumed.
  if (createFresh) {
    return {
      agent: await withTimeout(createCursorAgent(options, sdk), timeoutMs, "create"),
      history: "",
      replaced: true,
    };
  }
  try {
    return {
      agent: await withTimeout(sdk.resume(agentId, options), timeoutMs, "resume"),
      history: "",
      replaced: false,
    };
  } catch {
    return {
      agent: await withTimeout(createCursorAgent(options, sdk), timeoutMs, "create"),
      history: await recoveryHistory(runs.items, timeoutMs),
      replaced: true,
    };
  }
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

  const resumeStartedAt = performance.now();
  const agentId = agent.agentId;
  agent.close();
  const resumedAgent = await sdk.resume(agentId, {
    apiKey: process.env.CURSOR_API_KEY,
    model: modelSelection(request.model),
    local: {
      cwd: request.cwd,
      customTools: createCursorFilesystemTools(request.cwd, { readOnly: request.mode === "plan" }),
    },
  });
  emitTiming("agent_resume", resumeStartedAt);
  const finalSendStartedAt = performance.now();
  const run = await resumedAgent.send(message, options);
  emitTiming("send_after_resume", finalSendStartedAt);
  return { agent: resumedAgent, run };
}

function agentFingerprint(request) {
  const model = String(request?.model ?? "");
  const cwd = String(request?.cwd ?? "");
  const mode = request?.mode === "plan" ? "plan" : "agent";
  return `${model}\0${cwd}\0${mode}`;
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

      // Each user turn gets a fresh Cursor agent. Multi-turn continuity is carried by
      // slim memory (user prompts + conclusions), not by resuming full SDK checkpoints.
      if (agent) {
        agent.close();
        agent = undefined;
      }
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
      const { maxTokens: contextThreshold, maxChars: contextCharThreshold } = contextThresholdsForModel(request.model);
      // Build the SDK prompt before marking this turn pending, otherwise it would replay itself.
      // Persist first so even Agent.create/SDK initialization failures retain the user's request.
      const previousPendingTurn = memory.pendingTurn;
      let message = messageWithToolPolicy(messageWithSlimMemory(originalMessage, memory), { readOnly });
      memory.pendingTurn = pendingTurnContext(previousPendingTurn, originalMessage);
      await saveSlimMemory(memoryKey, memory).catch((error) => {
        process.stderr.write(`Cursor pending-turn persistence failed: ${error instanceof Error ? error.message : String(error)}\n`);
      });
      // Prefer idle prewarm Agent.create; refill immediately after consume.
      const acquireStartedAt = performance.now();
      const acquired = await prewarm.acquire(request);
      agent = acquired.agent;
      sendTiming("agent_acquire", acquireStartedAt, { source: acquired.source });
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
            sendPromptWithRecovery(agent, request, message, sendOptions),
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
          if (memory.contextStage === "full") {
            // Completed full-stage turns keep tools/assistant text but drop thinking, matching Vega.
            memory.fullTurns.push(formatCompletedTurn(originalMessage, state));
          }
          // A Cursor run can contain several model turns around tool calls. Bill the complete run,
          // but use only the latest model turn to estimate live context occupancy.
          const turnUsage = cursorRunUsage(result, accumulatedStreamUsage);
          const measuredTokens = contextTokensFromUsage(lastStreamUsage ?? turnUsage);
          if (memory.contextStage === "full") {
            const fullContextChars = formatSlimMemory(memory).length;
            if (memory.turns.length >= CURSOR_SLIM_MEMORY_TURNS
              || (measuredTokens > 0 && measuredTokens >= contextThreshold)
              || (measuredTokens === 0 && fullContextChars >= contextCharThreshold)) {
              // Stage one removes completed thinking/tool traces without summarizing conclusions.
              memory.contextStage = "slim";
              memory.contextTokens = 0;
              memory.fullTurns = [];
            } else {
              memory.contextTokens = measuredTokens;
            }
          } else {
            memory.contextTokens = measuredTokens;
          }
          await summarizeSlimMemory(memory, request).then((compacted) => {
            if (compacted) memory.contextTokens = 0;
          }).catch((error) => {
            process.stderr.write(`Cursor slim-memory compression failed: ${error instanceof Error ? error.message : String(error)}\n`);
          });
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
            reason: error instanceof CursorStartupTimeout ? "startup_timeout" : "retryable_error",
            continueWith: continueAfterOutput ? "go_on" : "same_prompt",
          });
          const recovery = await recoverTimedOutAgent(
            agent,
            activeRun,
            request,
            Agent,
            CURSOR_RECOVERY_TIMEOUT_MS,
            true,
          );
          agent = recovery.agent;
          // No-output retries keep the exact pre-pending prompt; after UI output, keep "go on".
          // Keep the stable slim-memory session key; do not promote ephemeral agent ids.
          send({ type: "ready", sessionId: sessionKey });
          activeRun = undefined;
        } finally {
          attemptActive = false;
          preserveActiveTurn = undefined;
        }
      }
    } catch (error) {
      send({ ok: false, error: error instanceof Error ? error.message : String(error) });
    } finally {
      activeRun = undefined;
      if (agent) {
        agent.close();
        agent = undefined;
      }
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
  contextThresholdsForModel,
  cursorContextWindow,
  parseCursorModelContextRules,
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
  agentCreateOptions,
  createAgentPrewarm,
  prewarmEnabled,
};

export {
  createCursorFilesystemTools,
  cursorBatchToolPolicy,
  cursorCavemanPolicy,
  cursorPromptPrefix,
} from "./cursor-filesystem-tools.mjs";
