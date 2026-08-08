#!/usr/bin/env node
// Submodular packing A/B: NOVA_CTX_SUBMODULAR=0 (legacy additive-ish greedy) vs =1 (submodular + Lazy Greedy).
// 用例聚焦“同关键词多文件冗余覆盖”场景——这是加性打分结构性缺陷最明显的场景：
// 多个文件命中同一批关键词时，旧实现会被 file_score 加性项重复奖励，新实现按 (term × file) 边际增益打包。
import { spawn } from "node:child_process";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const REPO = resolve(import.meta.dirname, "..");
const MODEL = process.argv.slice(2).find((arg) => !arg.startsWith("--")) || "opencode/deepseek-v4-flash";
const RUN_DIR = join(tmpdir(), `nova-submodular-ab-${process.pid}`);
const REPORT_JSON = join(REPO, "scripts", "submodular-ab.report.json");
const REPORT_MD = join(REPO, "scripts", "submodular-ab.report.md");
const NAPI_PATH = join(REPO, "src-tauri", "resources", "nova-tools-napi.node");

// 用例设计原则：每个 case 的关键词都会命中多个不同文件中的多个符号，
// 迫使打包器在“同 term 跨文件冗余”与“不同 term 多样性”之间做权衡。
// S1-S6 为合成用例；R1-R6 为真实会话回放 prompt（来源 ~/.nova/alkaid/sessions）。
const CASES = [
  {
    id: "S1",
    prompt: "用且只用一次 fast_context，keywords 取 fast_context_run、stem_rows、planned_terms，task 描述分析 fast_context 的检索归因与打包流程。只基于工具输出回答：哪些文件承担了归因、哪些承担了打包？不要再调用其它检索工具，不改代码。",
    keywordHint: /fast_context_run|stem_rows|planned_terms/i,
  },
  {
    id: "S2",
    prompt: "用且只用一次 fast_context，keywords 取 fast_context、find_symbols、callNapiTool，task 描述理解 MCP 工具从 JS 定义到 Rust 实现的完整链路。只基于工具输出回答，列出链路各环节所在文件。不要再检索，不改代码。",
    keywordHint: /fast_context|find_symbols|callNapiTool/i,
  },
  {
    id: "S3",
    prompt: "用且只用一次 fast_context，keywords 取 edit、applySmartEdits、candidate_starts，task 描述分析智能编辑的锚点定位策略。只基于工具输出回答定位顺序。不要再检索，不改代码。",
    keywordHint: /edit|applySmartEdits|candidate_starts/i,
  },
  {
    id: "S4",
    prompt: "用且只用一次 fast_context，keywords 取 search_text、search_text_scopes、discover_stems，task 描述理解 fast_context 内部两次 rg 扫描的分工。只基于工具输出回答。不要再检索，不改代码。",
    keywordHint: /search_text|search_text_scopes|discover_stems/i,
  },
  {
    id: "S5",
    prompt: "用且只用一次 fast_context，keywords 取 idle、timeout、session，task 描述分析会话 idle 超时从配置到 Agent 行为涉及哪些模块。只基于工具输出回答。不要再检索，不改代码。",
    keywordHint: /idle|timeout|session/i,
  },
  {
    id: "S6",
    prompt: "用且只用一次 fast_context，keywords 取 companion_test_files、co_changed_files、plan_terms_from_bodies，task 描述理解 fast_context 打包期的伴生测试与 git 共改耦合。只基于工具输出回答。不要再检索，不改代码。",
    keywordHint: /companion_test_files|co_changed_files|plan_terms_from_bodies/i,
  },
  // ---- 真实会话回放（来源 ~/.nova/alkaid/sessions）----
  {
    id: "R1",
    sourceSession: "bd12ca61-0c21-46f3-ad2d-cf85194a7a05",
    prompt: "真实任��回放：先用且只用一次 fast_context，keywords 取 remote、model、switch、session，task 描述理解远程控制会话与模型切换功能，分析在远程控制会话内支持切换模型需要改哪些地方。只基于工具输出回答，不再调用其它检索工具，不改代码。",
    keywordHint: /remote|model|switch|session/i,
  },
  {
    id: "R2",
    sourceSession: "lyra-18c9e043f96ce6b1",
    prompt: "真实任务回放：先用且只用一次 fast_context，keywords 取 kimi、moonshot、defaultConfig、providers，task 描述查找 /setup 或 alkaid 配置中为何默认出现 Kimi 和 GPT 的 provider 预设。只基于工具输出回答，不再检索，不改代码。",
    keywordHint: /kimi|moonshot|defaultConfig|providers/i,
  },
  {
    id: "R3",
    sourceSession: "c50e2312-3ba9-4696-a",
    prompt: "真实任务回放：先用且只用一次 fast_context，keywords 取 tool_start、usage、session_end、createAlkaidAgent、saveSession，task 描述分析会话生命周期中工具调用与 token 用量的记录链路。只基于工具输出回答，不再检索，不改代码。",
    keywordHint: /tool_start|usage|session_end|createAlkaidAgent|saveSession/i,
  },
  {
    id: "R4",
    sourceSession: "84cfea80-586d-499f-8",
    prompt: "真实任务回放：先用且只用一次 fast_context，keywords 取 证据链、画布、canvas、evidence，task 描述证据链画布组件的布局与渲染优化占屏空间，分析该怎么优化。只基于工具输出回答，不再检索，不改代码。",
    keywordHint: /证据链|画布|canvas|evidence/i,
  },
  {
    id: "R5",
    sourceSession: "60123777-76d9-4124-a",
    prompt: "真实任务回放：先用且只用一次 fast_context，keywords 取 edit_files、executeTools、parallel tool、applyEdit，task 描述查找 pi 工具执行代码判断 edit 是否并发执行。只基于工具输出回答，不再检索，不改代码。",
    keywordHint: /edit_files|executeTools|applyEdit/i,
  },
  {
    id: "R6",
    sourceSession: "38c5862a-1059-4c26-a",
    prompt: "真实任务回放：先用且只用一次 fast_context，keywords 取 fast_context、oldText、edit、virtual、anchor，task 描述分析 fast_context 与 edit 工具的锚点定位集成。只基于工具输出回答，不再检索，不改代码。",
    keywordHint: /fast_context|oldText|edit|anchor/i,
  },
];

function runCase(testCase, arm) {
  return new Promise((resolveCase) => {
    const env = {
      ...process.env,
      NOVA_TOOLS_NAPI_PATH: NAPI_PATH,
      NOVA_CTX_SUBMODULAR: arm === "B" ? "1" : "0",
      // 关闭在线学习干扰：本次只对比打包算法。
      NOVA_CONTEXT_LEARNING: "0",
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
    child.stdin.end(`${JSON.stringify({ prompt: testCase.prompt, model: MODEL, readOnly: true, cwd: REPO, sessionId: `submod-ab-${testCase.id}-${arm}-${Date.now()}` })}\n`);
  });
}

function summarize(result) {
  return {
    wallMs: result.wallMs,
    inputTokens: result.usage?.input ?? 0,
    outputTokens: result.usage?.output ?? 0,
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

mkdirSync(RUN_DIR, { recursive: true });
const evaluationStartedAt = Date.now();
const paired = await Promise.all(CASES.map(async (testCase) => {
  const [a, b] = await Promise.all([runCase(testCase, "A"), runCase(testCase, "B")]);
  return { id: testCase.id, prompt: testCase.prompt, A: summarize(a), B: summarize(b), finalTextTail: { A: a.finalText.slice(-300), B: b.finalText.slice(-300) } };
}));
const parallelElapsedMs = Date.now() - evaluationStartedAt;
const totals = {};
for (const arm of ["A", "B"]) {
  totals[arm] = Object.fromEntries(["wallMs", "inputTokens", "outputTokens", "totalTokens", "toolCalls", "fastContextCalls", "readCalls", "bashCalls", "toolTimeMs"].map((key) => [key, total(paired, arm, key)]));
}
const deltas = Object.fromEntries(Object.keys(totals.A).map((key) => [key, totals.B[key] - totals.A[key]]));
const report = { model: MODEL, ranAt: new Date().toISOString(), armA: "NOVA_CTX_SUBMODULAR=0 (legacy)", armB: "NOVA_CTX_SUBMODULAR=1 (submodular+lazy-greedy)", parallel: true, parallelElapsedMs, cases: paired, totals, deltas };
writeFileSync(REPORT_JSON, JSON.stringify(report, null, 2));

const lines = [
  "# 次模打包（Submodular + Lazy Greedy）A/B 报告",
  "",
  `- 模型：\`${MODEL}\``,
  `- A 臂（对照）：\`NOVA_CTX_SUBMODULAR=0\` — 粗粒度特征 + O(n²) 朴素贪心`,
  `- B 臂（实验）：\`NOVA_CTX_SUBMODULAR=1\` — (term × file) 细化特征 + Lazy Greedy`,
  `- 并行耗时：${(parallelElapsedMs / 1000).toFixed(1)}s`,
  "",
  "## 总计",
  "",
  "| 指标 | A（旧） | B（次模） | Δ | Δ% |",
  "|---|---:|---:|---:|---:|",
  ...["totalTokens", "inputTokens", "outputTokens", "toolCalls", "fastContextCalls", "readCalls", "bashCalls", "toolTimeMs", "wallMs"].map((key) => {
    const label = { totalTokens: "总 token", inputTokens: "输入 token", outputTokens: "输出 token", toolCalls: "工具调用", fastContextCalls: "fast_context", readCalls: "read", bashCalls: "bash", toolTimeMs: "工具耗时(ms)", wallMs: "端到端(ms)" }[key];
    return `| ${label} | ${totals.A[key]} | ${totals.B[key]} | ${deltas[key] > 0 ? "+" : ""}${deltas[key]} | ${percent(deltas[key], totals.A[key])} |`;
  }),
  "",
  "## 分案例",
  "",
];
for (const row of paired) {
  lines.push(`### ${row.id}`, "", row.prompt, "", "| Arm | Tokens | 工具调用 | fast_context | read | 工具耗时 | 端到端 | 序列 |", "|---|---:|---:|---:|---:|---:|---:|---|");
  for (const arm of ["A", "B"]) {
    const value = row[arm];
    lines.push(`| ${arm} | ${value.totalTokens} | ${value.toolCalls} | ${value.fastContextCalls} | ${value.readCalls} | ${value.toolTimeMs}ms | ${value.wallMs}ms | ${value.sequence || "无"} |`);
  }
  lines.push("", "**A 末尾**：" + row.finalTextTail.A.replaceAll("\n", " ").slice(0, 200), "", "**B 末尾**：" + row.finalTextTail.B.replaceAll("\n", " ").slice(0, 200), "");
}
lines.push("## 判读口径", "",
  "- **核心指标**：`readCalls`（fast_context 之后是否还需要补 read）。次模覆盖多样性更好 → read 应减少。",
  "- **次要指标**：`totalTokens` / `wallMs`。若 B 臂 read 减少但总 token 反而升高，说明打包更宽但单次信息密度下降，需权衡。",
  "- **反指标**：若 B 臂 fast_context 调用次数增加，说明单次打包不足、模型被迫二次检索——次模反而更差。",
  "- A 与 B 使用相同代码、提示词、模型和并发时刻；唯一差异是 `NOVA_CTX_SUBMODULAR=0/1`。",
);
writeFileSync(REPORT_MD, lines.join("\n"));
console.log(JSON.stringify({ report: REPORT_JSON, markdown: REPORT_MD, parallelElapsedMs, totals, deltas }, null, 2));
if (!process.env.NOVA_KEEP_AB_TEMP) rmSync(RUN_DIR, { recursive: true, force: true });
