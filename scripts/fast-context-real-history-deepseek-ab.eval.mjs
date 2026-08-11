// DeepSeek model-level A/B using real local Nova conversation history.
// A = indexed legacy fast_context; B = optimized index-free one-pass fast_context.
import { spawn } from "node:child_process";
import { existsSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join, resolve } from "node:path";

const REPO = resolve(import.meta.dirname, "..");
const EXE = process.env.LYRA_AB_EXE ?? join(REPO, ".cargo-target", "debug", "nova.exe");
const MODEL = process.env.LYRA_AB_MODEL ?? "opencode/deepseek-v4-flash/variant/high";
const THREAD_DIR = process.env.LYRA_AB_THREAD_DIR ?? join(homedir(), ".nova", "threads");
const OUT = process.env.LYRA_AB_OUT ?? join(import.meta.dirname, "fast-context-real-history-deepseek-ab.report.json");
const LIMIT = Number(process.env.LYRA_AB_CASES ?? 4);
const TIMEOUT_MS = Number(process.env.LYRA_AB_TIMEOUT_MS ?? 8 * 60_000);
const preferred = new Set((process.env.LYRA_AB_THREAD_IDS ?? [
  "e09fd327-9a4a-4a2d-bf80-965570bfedfa", // fastcontext performance
  "18be7005-8eea-445f-9938-9eb3f17562cd", // fastcontext git training
  "719d5639-3e22-4d03-83eb-193e5fa5fd33", // fastcontext latency
  "3989321a-b5be-4474-a52c-b2fb658ced23", // Lyra regression
].join(",")).split(",").filter(Boolean));

const asText = (item) => typeof item?.text === "string" ? item.text.trim() : "";
const toolName = (item) => `${item?.title ?? ""} ${item?.kind ?? ""}`.toLowerCase();
const parseRawInput = (item) => {
  if (item?.rawInput && typeof item.rawInput === "object") return item.rawInput;
  try { return JSON.parse(item?.rawInput ?? "{}"); } catch { return {}; }
};
const extractPaths = (text) => [...String(text).matchAll(/(?:### |DEF |EDIT |dependency[^\n]*?)([\w./\\-]+\.(?:rs|ts|tsx|js|mjs|json|md))(?::\d+)?/g)].map((m) => m[1].replaceAll("\\", "/"));

function loadCases() {
  const cases = [];
  if (!existsSync(THREAD_DIR)) throw new Error(`thread directory missing: ${THREAD_DIR}`);
  for (const file of readdirSync(THREAD_DIR).filter((f) => f.endsWith(".json"))) {
    let thread;
    try { thread = JSON.parse(readFileSync(join(THREAD_DIR, file), "utf8")); } catch { continue; }
    if (!preferred.has(thread.id)) continue;
    const items = thread.items ?? [];
    for (let i = 0; i < items.length; i++) {
      if (items[i]?.type !== "user" || !asText(items[i])) continue;
      const nextUser = items.findIndex((item, j) => j > i && item?.type === "user");
      const end = nextUser < 0 ? items.length : nextUser;
      const fc = items.slice(i + 1, end).find((item) => item?.type === "tool" && toolName(item).includes("fast_context"));
      if (!fc) continue;
      const input = parseRawInput(fc);
      const keywords = Array.isArray(input.keywords) ? input.keywords.filter((v) => typeof v === "string") : [];
      if (!keywords.length) continue;
      const originalOutput = JSON.stringify(fc.content ?? fc.rawOutput ?? "");
      const laterPaths = items.slice(i + 1, end).flatMap((item) => [
        ...(item.locations ?? []).map((location) => location.path).filter(Boolean),
        ...extractPaths(JSON.stringify(item.content ?? "")),
      ]);
      const historicalPaths = [...new Set([...extractPaths(originalOutput), ...laterPaths].map((p) => p.replaceAll("\\", "/")))].slice(0, 12);
      // Deleted fixtures and old-worktree paths are impossible to retrieve from this checkout.
      const expectedPaths = historicalPaths.filter((p) => existsSync(join(REPO, p)));
      const unavailablePaths = historicalPaths.filter((p) => !existsSync(join(REPO, p)));
      const history = items.slice(0, i).filter((item) => ["user", "assistant"].includes(item.type) && asText(item)).slice(-6)
        .map((item) => `${item.type === "user" ? "用户" : "助手"}: ${asText(item).slice(0, 1800)}`).join("\n\n");
      cases.push({ id: `${thread.id.slice(0, 8)}-${i}`, threadId: thread.id, title: thread.title, user: asText(items[i]), history, keywords, historicalPaths, expectedPaths, unavailablePaths });
      break;
    }
  }
  return cases.sort((a, b) => Number(preferred.has(b.threadId)) - Number(preferred.has(a.threadId))).slice(0, LIMIT);
}

function runOne(test, arm) {
  return new Promise((resolveRun) => {
    const started = Date.now();
    const child = spawn(EXE, ["lyra"], {
      cwd: REPO,
      env: { ...process.env, NOVA_DATA_DIR: join(homedir(), ".nova"), NOVA_CONTEXT_RETRIEVAL_MODE: arm === "A" ? "fast" : "super", LYRA_SPECULATE: "0" },
      stdio: ["pipe", "pipe", "pipe"],
    });
    let buffer = "", stderr = "", finalText = "", contextText = "", usage = null, done = false;
    const finish = (extra = {}) => {
      if (done) return; done = true; clearTimeout(timer); try { child.kill(); } catch {}
      const evidence = `${contextText}\n${finalText}`;
      const keywordHits = test.keywords.filter((term) => evidence.toLowerCase().includes(term.toLowerCase()));
      const pathHits = test.expectedPaths.filter((term) => evidence.toLowerCase().includes(term.toLowerCase()));
      const retrievalMs = Number(contextText.match(/# scan:[\s\S]*?\/ ([\d.]+)ms/)?.[1] ?? NaN);
      resolveRun({ arm, wallMs: Date.now() - started, retrievalMs: Number.isFinite(retrievalMs) ? retrievalMs : null, finalText, contextChars: contextText.length, keywordRecall: keywordHits.length / test.keywords.length, pathRecall: test.expectedPaths.length ? pathHits.length / test.expectedPaths.length : null, keywordHits, pathHits, usage, stderr: stderr.slice(-800), ...extra });
    };
    const timer = setTimeout(() => finish({ timeout: true }), TIMEOUT_MS);
    child.stdout.on("data", (chunk) => {
      buffer += chunk; let newline;
      while ((newline = buffer.indexOf("\n")) >= 0) {
        const line = buffer.slice(0, newline); buffer = buffer.slice(newline + 1); let event;
        try { event = JSON.parse(line); } catch { continue; }
        if (event.type === "item" && event.item?.type === "agent_message") finalText = event.item.text ?? finalText;
        if (event.type === "item" && event.item?.type === "mcp_tool_call") {
          const serialized = JSON.stringify(event.item);
          if (serialized.includes("fast_context")) {
            contextText = serialized;
            const result = event.item.result?.content;
            if (Array.isArray(result)) {
              const direct = result.map((entry) => entry?.text ?? entry?.content?.text ?? "").join("\n");
              if (direct) contextText = direct;
            }
          }
        }
        if (event.type === "done") { usage = event.usage ?? null; finish(); }
        if (event.ok === false) finish({ error: event.error ?? event });
      }
    });
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.on("error", (error) => finish({ error: String(error) }));
    child.on("close", (code) => { if (!done) finish({ exitCode: code }); });
    const prompt = `以下来自一个真实 Nova 软件工程会话。请延续上下文处理最后的真实用户请求。\n\n${test.history ? `历史：\n${test.history}\n\n` : ""}当前用户：${test.user}\n\n评测约束：只调用一次 fast_context，不调用 read/find_symbols/bash/edit/write；基于返回内容给出分析，不实际修改文件。fast_context 使用原会话检索意图：keywords=${JSON.stringify(test.keywords)}，task=${JSON.stringify(test.user.slice(0, 300))}。`;
    child.stdin.end(`${JSON.stringify({ action: "prompt", cwd: REPO, mode: "plan", model: MODEL, sessionId: `real-history-fc-${test.id}-${arm}-${Date.now()}`, parts: [{ type: "text", text: prompt }] })}\n`);
  });
}

const cases = loadCases();
if (!cases.length) throw new Error("no eligible real-history cases found");
const rows = [];
for (const test of cases) {
  process.stderr.write(`[${rows.length + 1}/${cases.length}] ${test.title}\n`);
  const [A, B] = await Promise.all([runOne(test, "A"), runOne(test, "B")]);
  rows.push({ test, A, B });
}
const tokenCount = (run) => Object.values(run.usage ?? {}).filter(Number.isFinite).reduce((a, b) => a + b, 0);
const aggregate = (arm) => {
  const values = rows.map((row) => row[arm]);
  const validRetrieval = values.map((run) => run.retrievalMs).filter(Number.isFinite);
  const average = (items) => items.length ? items.reduce((sum, value) => sum + value, 0) / items.length : null;
  return {
    cases: values.length,
    success: values.filter((run) => !run.timeout && !run.error && !run.exitCode).length,
    avgWallMs: average(values.map((run) => Number(run.wallMs ?? 0))),
    avgRetrievalMs: average(validRetrieval),
    avgKeywordRecall: average(values.map((run) => Number(run.keywordRecall ?? 0))),
    avgPathRecall: average(values.map((run) => Number(run.pathRecall ?? 0))),
    totalTokens: values.reduce((sum, run) => sum + tokenCount(run), 0),
  };
};
const report = { ranAt: new Date().toISOString(), model: MODEL, source: THREAD_DIR, arms: { A: "indexed legacy", B: "optimized no-index query-driven slice" }, rows, totals: { A: aggregate("A"), B: aggregate("B") } };
writeFileSync(OUT, JSON.stringify(report, null, 2));
console.log(JSON.stringify({ out: OUT, totals: report.totals }, null, 2));
