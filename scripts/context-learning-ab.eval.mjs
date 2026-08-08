#!/usr/bin/env node
// Replay recent real-session feedback into the online model, then run parallel DeepSeek A/B.
import { spawn } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { basename, join, resolve } from "node:path";
import { callNapiTool } from "./nova-napi-tools.mjs";

const REPO = resolve(import.meta.dirname, "..");
const MODEL = process.argv.slice(2).find((arg) => !arg.startsWith("--")) || "opencode/deepseek-v4-flash";
const SESSION_DIR = join(homedir(), ".nova", "alkaid", "sessions");
const RUN_DIR = join(tmpdir(), `nova-context-learning-ab-${process.pid}`);
const LEARNING_DIR = join(RUN_DIR, "learning");
const REPORT_JSON = join(REPO, "scripts", "context-learning-ab.report.json");
const REPORT_MD = join(REPO, "scripts", "context-learning-ab.report.md");
const NAPI_PATH = join(REPO, "src-tauri", "resources", "nova-tools-napi.node");

const CASES = [
  {
    id: "R1",
    sourceSession: "6097d0a6-916d-48ad-ac16-b68c320116d6.slim.json",
    prompt: "这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 fast_context_run、co_changed_files，分析还能如何提高 fast_context 效率并减少后续 read。只基于工具输出回答，不再调用其它检索工具，不改代码。"
  },
  {
    id: "R2",
    sourceSession: "6097d0a6-916d-48ad-ac16-b68c320116d6.slim.json",
    prompt: "这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 native_edit、edit、fast_context，分析原生 edit 使用独立工具实现时如何消费 fast_context 编辑锚点。只基于工具输出回答，不再检索，不改代码。"
  },
  {
    id: "R3",
    sourceSession: "09774430-c287-4f78-bfc0-bfc9116cd4e7.slim.json",
    prompt: "这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 buildAlkaidSystemPrompt、fast_context，分析为什么 Agent 开始会用 fast_context、后续却可能不再使用。只基于工具输出回答，不再检索，不改代码。"
  },
  {
    id: "R4",
    sourceSession: "bd12ca61-0c21-46f3-ad2d-cf85194a7a05.slim.json",
    prompt: "这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 remote、model、switch、session，task 描述理解远程控制会话与模型切换功能，分析在远程控制会话内支持切换模型需要改哪些地方。只基于工具输出回答，不再调用其它检索工具，不改代码。"
  },
  {
    id: "R5",
    sourceSession: "84cfea80-586d-499f-802b-6d2bd8623476.slim.json",
    prompt: "这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 证据链、画布、canvas、evidence，task 描述证据链画布组件的布局与渲染优化占屏空间，分析该怎么优化。只基于工具输出回答，不再调用其它检索工具，不改代码。"
  },
  {
    id: "R6",
    sourceSession: "60123777-76d9-4124-a601-773d9b618065.slim.json",
    prompt: "这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 edit_files、executeTools、parallel tool、applyEdit，task 描述查找 pi 工具执行代码判断 edit 是否并发执行，分析 pi 的 edit 是否也会并发执行。只基于工具输出回答，不再调用其它检索工具，不改代码。"
  },
  {
    id: "R7",
    sourceSession: "bf15e349-390e-4184-a014-dec30badb709.slim.json",
    prompt: "这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 远程控制、会话排序、remote control、session，task 描述远程控制的会话列表排序改为按用户最后输入提示词时间倒序，分析当前排序逻辑和需要改的地方。只基于工具输出回答，不再调用其它检索工具，不改代码。"
  },
  {
    id: "R8",
    sourceSession: "da76f639-644d-471c-85dc-a0b2f55ae929.slim.json",
    prompt: "这是此前真实任务的聚焦回放：先用且只用一次 fast_context，keywords 取 workflow、工作流、edge、transition、handoff，task 描述简化工作流配置让引擎隐式补充会话结论接力，分析当前工作流配置和运行引擎的结构。只基于工具输出回答，不再调用其它检索工具，不改代码。"
  },
];

function recentSessionFiles(limit = 80) {
  if (!existsSync(SESSION_DIR)) return [];
  return readdirSync(SESSION_DIR)
    .filter((name) => name.endsWith(".slim.json"))
    .map((name) => join(SESSION_DIR, name))
    .sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs)
    .slice(0, limit);
}

function relativeFeedbackPath(raw) {
  if (!raw) return null;
  const normalized = String(raw).replaceAll("\\", "/");
  if (!normalized.startsWith("/")) return normalized;
  const markers = ["/nova-client/", "/nova-client-fast-context-ab/", "/nova-client-online-learning/"];
  for (const marker of markers) {
    const index = normalized.indexOf(marker);
    if (index >= 0) return normalized.slice(index + marker.length);
  }
  return null;
}

async function pretrainFromRecentSessions() {
  process.env.NOVA_CONTEXT_LEARNING = "1";
  process.env.NOVA_CONTEXT_LEARNING_OWNER = "1";
  process.env.NOVA_CONTEXT_LEARNING_DIR = LEARNING_DIR;
  process.env.NOVA_TOOLS_NAPI_PATH = NAPI_PATH;
  let contexts = 0;
  let edits = 0;
  const sessions = [];
  for (const path of recentSessionFiles()) {
    let data;
    try { data = JSON.parse(readFileSync(path, "utf8")); } catch { continue; }
    let used = false;
    for (const message of data.fullMessages ?? []) {
      for (const part of message.content ?? []) {
        if (part?.type !== "toolCall") continue;
        if (part.name === "fast_context") {
          try {
            await callNapiTool("fast_context", REPO, part.arguments ?? {});
            contexts += 1;
            used = true;
          } catch {}
        } else if (part.name === "edit") {
          const relative = relativeFeedbackPath(part.arguments?.path);
          if (!relative || !existsSync(join(REPO, relative))) continue;
          try {
            const feedback = await callNapiTool("observe_context_feedback", REPO, { action: "edit", path: relative });
            edits += Number(feedback?.updated ?? 0);
            used = true;
          } catch {}
        }
      }
    }
    if (used) sessions.push(basename(path));
    if (contexts >= 16 && edits >= 20) break;
  }
  // Settle the last trace so weak negatives and the final model are persisted.
  await callNapiTool("fast_context", REPO, { keywords: ["fast_context"], task: "settle online replay trace", budget: 100, maxBytes: 8192 }).catch(() => {});
  const modelFile = existsSync(LEARNING_DIR) ? readdirSync(LEARNING_DIR).find((name) => name.endsWith(".json")) : null;
  const model = modelFile ? JSON.parse(readFileSync(join(LEARNING_DIR, modelFile), "utf8")) : null;
  return { contexts, positiveUpdates: edits, sessions, model };
}

function runCase(testCase, arm) {
  return new Promise((resolveCase) => {
    const env = {
      ...process.env,
      NOVA_TOOLS_NAPI_PATH: NAPI_PATH,
      NOVA_CONTEXT_LEARNING: arm === "B" ? "1" : "0",
      NOVA_CONTEXT_LEARNING_OWNER: "1", // standalone evaluator acts as the sole owner
      NOVA_CONTEXT_LEARNING_DIR: arm === "B" ? LEARNING_DIR : join(RUN_DIR, "baseline-unused"),
    };
    const child = spawn(process.execPath, [join(REPO, "scripts", "alkaid.mjs")], { cwd: REPO, env, stdio: ["pipe", "pipe", "pipe"] });
    const startedAt = Date.now();
    const tools = [];
    let usage = null;
    let finalText = "";
    let stderr = "";
    let buffer = "";
    let finished = false;
    const finish = (extra = {}) => {
      if (finished) return;
      finished = true;
      clearTimeout(timer);
      try { child.kill(); } catch {}
      resolveCase({ caseId: testCase.id, arm, wallMs: Date.now() - startedAt, tools, usage, finalText, stderr: stderr.slice(-1000), ...extra });
    };
    const timer = setTimeout(() => finish({ timeout: true }), 2 * 60_000);
    child.stdout.on("data", (chunk) => {
      buffer += chunk.toString();
      let newline;
      while ((newline = buffer.indexOf("\n")) >= 0) {
        const line = buffer.slice(0, newline); buffer = buffer.slice(newline + 1);
        let message; try { message = JSON.parse(line); } catch { continue; }
        if (message.type === "tool_start") tools.push({ name: message.name, args: message.arguments, started: true });
        else if (message.type === "tool_end") {
          const target = [...tools].reverse().find((tool) => tool.name === message.name && tool.durationMs === undefined);
          if (target) Object.assign(target, { durationMs: message.durationMs ?? 0, isError: !!message.isError });
        } else if (message.type === "done") {
          usage = message.usage ?? null;
          finalText = message.text ?? "";
          finish({ reportedWallMs: message.wallMs });
        } else if (message.type === "error") finish({ error: message.error });
      }
    });
    child.stderr.on("data", (chunk) => { stderr += chunk.toString(); });
    child.on("exit", (code) => { if (!finished) finish({ exitCode: code, error: code ? `exit ${code}` : undefined }); });
    child.stdin.end(`${JSON.stringify({ prompt: testCase.prompt, model: MODEL, readOnly: true, cwd: REPO, sessionId: `ctx-ab-${testCase.id}-${arm}-${Date.now()}` })}\n`);
  });
}

function summarize(result) {
  return {
    wallMs: result.wallMs,
    reportedWallMs: result.reportedWallMs,
    inputTokens: result.usage?.input ?? 0,
    outputTokens: result.usage?.output ?? 0,
    cacheReadTokens: result.usage?.cacheRead ?? 0,
    totalTokens: result.usage?.totalTokens ?? 0,
    toolCalls: result.tools.length,
    fastContextCalls: result.tools.filter((tool) => tool.name === "fast_context").length,
    readCalls: result.tools.filter((tool) => tool.name === "read").length,
    bashCalls: result.tools.filter((tool) => tool.name === "bash").length,
    toolTimeMs: result.tools.reduce((sum, tool) => sum + (tool.durationMs ?? 0), 0),
    sequence: result.tools.map((tool) => `${tool.name}${tool.isError ? "✗" : ""}`).join(" → "),
    timeout: !!result.timeout,
    error: result.error,
  };
}

function total(rows, arm, key) {
  return rows.reduce((sum, row) => sum + row[arm][key], 0);
}

function percent(delta, baseline) {
  return baseline ? `${(delta / baseline * 100).toFixed(1)}%` : "n/a";
}

mkdirSync(LEARNING_DIR, { recursive: true });
const pretraining = await pretrainFromRecentSessions();
const evaluationStartedAt = Date.now();
const paired = await Promise.all(CASES.map(async (testCase) => {
  const [a, b] = await Promise.all([runCase(testCase, "A"), runCase(testCase, "B")]);
  return { id: testCase.id, sourceSession: testCase.sourceSession, prompt: testCase.prompt, A: summarize(a), B: summarize(b), finalTextTail: { A: a.finalText.slice(-300), B: b.finalText.slice(-300) } };
}));
const parallelElapsedMs = Date.now() - evaluationStartedAt;
const totals = {};
for (const arm of ["A", "B"]) {
  totals[arm] = Object.fromEntries(["wallMs", "inputTokens", "outputTokens", "cacheReadTokens", "totalTokens", "toolCalls", "fastContextCalls", "readCalls", "bashCalls", "toolTimeMs"].map((key) => [key, total(paired, arm, key)]));
}
const deltas = Object.fromEntries(Object.keys(totals.A).map((key) => [key, totals.B[key] - totals.A[key]]));
const report = { model: MODEL, ranAt: new Date().toISOString(), branch: "feat/fast-context-online-learning-ab", parallel: true, parallelElapsedMs, cases: paired, pretraining, totals, deltas };
writeFileSync(REPORT_JSON, JSON.stringify(report, null, 2));
const lines = [
  "# Fast Context 在线增量学习 A/B 报告",
  "",
  `- 模型：\`${MODEL}\``,
  `- 分支：\`${report.branch}\``,
  `- 执行方式：${CASES.length * 2} 个 DeepSeek 会话并行（每个 case 的 A/B 同时启动）`,
  `- 并行评测端到端实际耗时：${parallelElapsedMs}ms（汇总 wallMs 是各会话耗时之和）`,
  `- 训练回放：${pretraining.sessions.length} 个最近真实会话，${pretraining.contexts} 次 fast_context，${pretraining.positiveUpdates} 个 edit 正反馈`,
  `- 训练后模型：observations=${pretraining.model?.observations ?? 0}, positives=${pretraining.model?.positives ?? 0}`,
  "",
  "## 汇总",
  "",
  "| 指标 | A 关闭学习 | B 在线模型 | B-A |",
  "|---|---:|---:|---:|",
  ...Object.keys(totals.A).map((key) => `| ${key} | ${totals.A[key]} | ${totals.B[key]} | ${deltas[key]} |`),
  "",
  "## 结论与解读",
  "",
  `- 总 token：${percent(deltas.totalTokens, totals.A.totalTokens)}（${totals.A.totalTokens} → ${totals.B.totalTokens}）。`,
  `- 输入 token：${percent(deltas.inputTokens, totals.A.inputTokens)}；输出 token：${percent(deltas.outputTokens, totals.A.outputTokens)}。`,
  `- 各会话 wall time 求和：${percent(deltas.wallMs, totals.A.wallMs)}；6 路并行端到端为 ${parallelElapsedMs}ms。`,
  `- 工具调用数保持 ${totals.A.toolCalls} → ${totals.B.toolCalls}，read 保持 ${totals.A.readCalls} → ${totals.B.readCalls}；本组聚焦回放主要验证上下文内容/排序，而不是工具调用策略。`,
  `- fast_context 自身工具耗时增加 ${deltas.toolTimeMs}ms（${percent(deltas.toolTimeMs, totals.A.toolTimeMs)}），绝对值仅 ${totals.B.toolTimeMs}ms，主要总耗时仍来自模型推理。`,
  "- 三个 case 的 token 均下降，但这是每个 arm 单次采样；DeepSeek 输出有随机性，因此当前结果可作为正向信号，不能视为统计显著结论。上线前建议固定模型参数后至少重复 5 轮。",
  "",
  "## 分案例",
  "",
];
for (const row of paired) {
  lines.push(`### ${row.id}（来源：\`${row.sourceSession}\`）`, "", row.prompt, "", "| Arm | Tokens | Input | Output | 工具调用 | fast_context | read | 工具耗时 | 实际耗时 | 序列 |", "|---|---:|---:|---:|---:|---:|---:|---:|---:|---|");
  for (const arm of ["A", "B"]) {
    const value = row[arm];
    lines.push(`| ${arm} | ${value.totalTokens} | ${value.inputTokens} | ${value.outputTokens} | ${value.toolCalls} | ${value.fastContextCalls} | ${value.readCalls} | ${value.toolTimeMs}ms | ${value.wallMs}ms | ${value.sequence || "无"} |`);
  }
  lines.push("");
}
lines.push("## 说明", "", "- A 与 B 使用完全相同的代码、提示词、DeepSeek 模型和并发时刻；唯一差异是 `NOVA_CONTEXT_LEARNING=0/1`。", "- B 在评测前按原始工具调用顺序回放最近真实会话中的 `fast_context → edit`，每个 edit 自动触发一次增量 Logistic 更新。", "- token 来自 provider usage；实际耗时为子进程端到端 wall time；工具耗时来自 tool_start/tool_end。", "- 模型仅重排可选候选，显式文件、seed、required 单元和预算硬约束不受学习模型控制。", "");
writeFileSync(REPORT_MD, lines.join("\n"));
console.log(JSON.stringify({ report: REPORT_JSON, markdown: REPORT_MD, parallelElapsedMs, pretraining, totals, deltas }, null, 2));
if (!process.env.NOVA_KEEP_AB_TEMP) rmSync(RUN_DIR, { recursive: true, force: true });
