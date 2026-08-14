// DeepSeek model-level A/B using real local Nova conversation history.
// A = baseline executable; B = current native resident FastContext executable.
import { spawn } from "node:child_process";
import { existsSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join, resolve } from "node:path";

const REPO = resolve(import.meta.dirname, "..");
const EXE = process.env.LYRA_AB_EXE ?? join(REPO, ".cargo-target", "debug", "nova.exe");
const EXE_A = process.env.LYRA_AB_EXE_A ?? EXE;
const EXE_B = process.env.LYRA_AB_EXE_B ?? EXE;
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

function builtInCases() {
  return [
    {
      id: "resident-service-routing",
      threadId: "builtin",
      title: "原生常驻服务路由",
      user: "分析 FastContext 原生常驻进程如何让 Lyra 直连、其它后端通过 bridge 接入，并指出关键调用链。",
      history: "",
      keywords: ["ContextService", "preload_indexes", "callGlobalContextTool", "fast_context"],
      expectedPaths: ["src-tauri/src/context_service.rs", "src-tauri/src/lyra/tools.rs", "scripts/nova-context-client.mjs"],
    },
    {
      id: "mmap-incremental-index",
      threadId: "builtin",
      title: "mmap 增量索引",
      user: "分析 FastContext 索引如何在启动时 mmap 加载、运行时只使用内存并增量更新。",
      history: "",
      keywords: ["preload_indexes", "with_mapped_file", "load_cache", "store_cache"],
      expectedPaths: ["src-tauri/src/nova_tools_native/context.rs"],
    },
    {
      id: "super-fallback",
      threadId: "builtin",
      title: "SuperContext 回退",
      user: "确认 SuperContext 隐藏后，旧 super 配置如何迁移并回退到 FastContext。",
      history: "",
      keywords: ["ContextRetrievalMode", "super_context_enabled", "no_index_context_enabled"],
      expectedPaths: ["src-tauri/src/settings.rs", "src/components/SettingsModal.tsx", "src-tauri/src/nova_tools_native/context.rs"],
    },
    {
      id: "reverse-import-graph",
      threadId: "builtin",
      title: "反向 import 图兼容性",
      user: "分析 FastContext 的反向 import 图缓存和别名、barrel 调用方发现链路。",
      history: "",
      keywords: ["reverse_from_files", "ReverseMap", "discover"],
      expectedPaths: ["src-tauri/src/nova_tools_native/context.rs"],
    },
  ];
}

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
      const expectedPaths = [...new Set([...extractPaths(originalOutput), ...laterPaths].map((p) => p.replaceAll("\\", "/")))].slice(0, 8);
      const history = items.slice(0, i).filter((item) => ["user", "assistant"].includes(item.type) && asText(item)).slice(-6)
        .map((item) => `${item.type === "user" ? "用户" : "助手"}: ${asText(item).slice(0, 1800)}`).join("\n\n");
      cases.push({ id: `${thread.id.slice(0, 8)}-${i}`, threadId: thread.id, title: thread.title, user: asText(items[i]), history, keywords, expectedPaths });
      break;
    }
  }
  const selected = cases.sort((a, b) => Number(preferred.has(b.threadId)) - Number(preferred.has(a.threadId))).slice(0, LIMIT);
  return selected.length ? selected : builtInCases().slice(0, LIMIT);
}

function runOne(test, arm) {
  return new Promise((resolveRun) => {
    const started = Date.now();
    const child = spawn(arm === "A" ? EXE_A : EXE_B, ["lyra"], {
      cwd: REPO,
      env: { ...process.env, NOVA_DATA_DIR: join(homedir(), ".nova"), NOVA_CONTEXT_RETRIEVAL_MODE: "fast", NOVA_CONTEXT_NO_INDEX: "0", LYRA_SPECULATE: "0" },
      stdio: ["pipe", "pipe", "pipe"],
    });
    let buffer = "", stderr = "", finalText = "", contextText = "", usage = null, done = false;
    const finish = (extra = {}) => {
      if (done) return; done = true; clearTimeout(timer); try { child.kill(); } catch {}
      const evidence = `${contextText}\n${finalText}`;
      const keywordHits = test.keywords.filter((term) => evidence.toLowerCase().includes(term.toLowerCase()));
      const pathHits = test.expectedPaths.filter((term) => evidence.toLowerCase().includes(term.toLowerCase()));
      resolveRun({ arm, wallMs: Date.now() - started, finalText, contextChars: contextText.length, keywordRecall: keywordHits.length / test.keywords.length, pathRecall: test.expectedPaths.length ? pathHits.length / test.expectedPaths.length : null, keywordHits, pathHits, usage, stderr: stderr.slice(-800), ...extra });
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
          if (serialized.includes("fast_context")) contextText += serialized;
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
  const runs = rows.map((row) => row[arm]);
  const avg = (key) => runs.reduce((sum, run) => sum + Number(run[key] ?? 0), 0) / runs.length;
  return { cases: runs.length, success: runs.filter((run) => !run.timeout && !run.error && !run.exitCode).length, avgWallMs: avg("wallMs"), avgKeywordRecall: avg("keywordRecall"), avgPathRecall: avg("pathRecall"), totalTokens: runs.reduce((sum, run) => sum + tokenCount(run), 0) };
};
const report = { ranAt: new Date().toISOString(), model: MODEL, source: THREAD_DIR, arms: { A: EXE_A, B: EXE_B }, contextMode: "fast", rows, totals: { A: aggregate("A"), B: aggregate("B") } };
writeFileSync(OUT, JSON.stringify(report, null, 2));
console.log(JSON.stringify({ out: OUT, totals: report.totals }, null, 2));
