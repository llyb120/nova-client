// fast-context-dsv4.eval.mjs — 真实模型（默认 deepseek/deepseek-v4-flash）对 fast_context 的使用行为评测
//
// 用法: node scripts/fast-context-dsv4.eval.mjs [model] [--case C1,C3] [--json]
//
// 每个用例起一个真实 alkaid 会话（readOnly，cwd=仓库根），记录全部 tool_start/tool_end 事件，
// 按用例期望打分：
//   - trigger      : 该调 fast_context 时是否调了（且在第一次检索动作之前/之中）
//   - no-over      : 不该调时是否保持克制（已知路径直接 read）
//   - params       : 参数是否合法（keywords 1–5、files ≤6、budget 100–1200、无工具报错）
//   - no-research  : fast_context 之后是否又用 rg/grep/git grep 重复检索（违反硬性约束）
//   - no-reread    : fast_context 之后是否立即重读输出中已完整展示的文件段（宽松：仅统计）
// 结果写 scripts/fast-context-dsv4.report.json，stdout 打表。

import { spawn } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = join(HERE, "..");
const SEARCH_RE = /(^|\s|\/|\|)(rg|grep|git\s+grep|find|ag|fd)(\s|$)/;

const CASES = [
  {
    id: "C1",
    kind: "trigger",
    prompt: "我要给 fast_context 工具增加一个按目录过滤的新参数 includeGlobs。请分析从 JS 工具定义到 Rust native 实现需要改哪些地方，给出修改清单，不要改代码。",
    expect: { fastContext: true, maxRgAfterFc: 0 },
    keywordHint: /fast_context|includeGlobs/i,
  },
  {
    id: "C2",
    kind: "trigger",
    prompt: "分析会话 idle 超时（idle timeout）从配置项到 Agent 行为涉及哪些模块，如果要把默认超时从 120s 调到 300s 需要改哪些文件。不要改代码。",
    expect: { fastContext: true },
    keywordHint: /idle|timeout/i,
  },
  {
    id: "C3",
    kind: "no-over",
    prompt: "scripts/alkaid-core.mjs 第 25-35 行附近定义了几个字节数常量，分别是什么含义？只回答，不要查其它地方。",
    expect: { fastContext: false },
  },
  {
    id: "C4",
    kind: "find-symbols-ok",
    prompt: "createAlkaidAgent 在哪些地方被调用？只要文件和行号，不要正文。",
    expect: { fastContextOrFindSymbols: true },
  },
  {
    id: "C5",
    kind: "no-research",
    prompt: "先用 fast_context 查 clampToolOutputText 相关的完整单元，然后仅基于它的输出总结这个函数的职责与调用方，之后不许再用 rg/grep 检索。",
    expect: { fastContext: true, maxRgAfterFc: 0 },
    keywordHint: /clampToolOutputText/i,
  },
  {
    id: "C6",
    kind: "params",
    prompt: "了解 edit_files 智能编辑的锚点定位策略是怎么实现的，给我讲清楚逐级定位的顺序。不要改代码。",
    expect: { fastContext: true },
    keywordHint: /edit_files|applySmartEdits|定位|锚点/i,
  },
];

function runCase(model, testCase) {
  return new Promise((resolveCase) => {
    const child = spawn(process.execPath, [join(HERE, "alkaid.mjs")], {
      cwd: REPO,
      stdio: ["pipe", "pipe", "inherit"],
    });
    const startedAt = Date.now();
    const tools = [];
    let finalText = "";
    let buf = "";
    let finished = false;
    const finish = (extra = {}) => {
      if (finished) return;
      finished = true;
      clearTimeout(timer);
      try { child.kill(); } catch {}
      resolveCase({ id: testCase.id, tools, finalText, wallMs: Date.now() - startedAt, ...extra });
    };
    const timer = setTimeout(() => finish({ timeout: true }), 8 * 60 * 1000);
    child.stdout.on("data", (chunk) => {
      buf += chunk.toString();
      let idx;
      while ((idx = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, idx).trim();
        buf = buf.slice(idx + 1);
        if (!line) continue;
        let msg;
        try { msg = JSON.parse(line); } catch { continue; }
        if (msg.type === "tool_start") {
          tools.push({ name: msg.name, args: msg.arguments ?? {}, isError: null });
        } else if (msg.type === "tool_end") {
          const open = [...tools].reverse().find((t) => t.isError === null);
          if (open) open.isError = !!msg.isError;
        } else if (msg.type === "done") {
          finalText = msg.text ?? "";
          finish();
        } else if (msg.type === "error") {
          finish({ error: msg.error });
        }
      }
    });
    child.on("exit", () => finish());
    child.stdin.write(JSON.stringify({
      prompt: testCase.prompt,
      model,
      readOnly: true,
      cwd: REPO,
    }) + "\n");
    child.stdin.end();
  });
}

function isSearchCall(tool) {
  if (tool.name === "bash") return SEARCH_RE.test(String(tool.args?.command ?? ""));
  return /grep|glob|search/.test(tool.name);
}

function score(testCase, result) {
  const checks = [];
  const fcIndex = result.tools.findIndex((t) => t.name === "fast_context");
  const fcCalls = result.tools.filter((t) => t.name === "fast_context");
  const fsCalls = result.tools.filter((t) => t.name === "find_symbols");
  const firstSearchIndex = result.tools.findIndex((t) => t.name !== "fast_context" && (t.name === "read" || t.name === "find_symbols" || isSearchCall(t)));

  if (testCase.expect.fastContext) {
    checks.push({ name: "调用了 fast_context", pass: fcIndex >= 0 });
    if (fcIndex >= 0) {
      checks.push({ name: "fast_context 不晚于首个 read/检索动作", pass: firstSearchIndex < 0 || fcIndex <= firstSearchIndex });
      if (testCase.keywordHint) {
        const kw = fcCalls[0]?.args?.keywords;
        checks.push({ name: `keywords 命中主题 (${testCase.keywordHint})`, pass: Array.isArray(kw) && kw.some((k) => testCase.keywordHint.test(k)) });
      }
    }
  }
  if (testCase.expect.fastContext === false) {
    checks.push({ name: "未误触发 fast_context（直接 read）", pass: fcIndex < 0 });
    checks.push({ name: "确实用了 read", pass: result.tools.some((t) => t.name === "read") });
  }
  if (testCase.expect.fastContextOrFindSymbols) {
    checks.push({ name: "用了 fast_context 或 find_symbols", pass: fcCalls.length + fsCalls.length > 0 });
  }

  for (const [i, call] of fcCalls.entries()) {
    const a = call.args ?? {};
    const problems = [];
    if (a.keywords !== undefined && (!Array.isArray(a.keywords) || a.keywords.length < 1 || a.keywords.length > 5)) problems.push(`keywords=${JSON.stringify(a.keywords)}`);
    if (a.files !== undefined && (!Array.isArray(a.files) || a.files.length > 6)) problems.push(`files=${JSON.stringify(a.files)}`);
    if (a.budget !== undefined && (!Number.isInteger(a.budget) || a.budget < 100 || a.budget > 1200)) problems.push(`budget=${a.budget}`);
    if (a.maxBytes !== undefined && (!Number.isInteger(a.maxBytes) || a.maxBytes < 8192 || a.maxBytes > 65536)) problems.push(`maxBytes=${a.maxBytes}`);
    if (call.isError) problems.push("工具返回错误");
    checks.push({ name: `fast_context#${i + 1} 参数合法且无错误`, pass: problems.length === 0, detail: problems.join("; ") });
  }

  if (testCase.expect.maxRgAfterFc !== undefined && fcIndex >= 0) {
    const searchesAfter = result.tools.slice(fcIndex + 1).filter(isSearchCall);
    checks.push({ name: "fast_context 之后零 rg/grep 重复检索", pass: searchesAfter.length <= testCase.expect.maxRgAfterFc, detail: searchesAfter.map((s) => String(s.args?.command ?? s.name).slice(0, 80)).join(" | ") });
  }

  const readsAfter = fcIndex >= 0 ? result.tools.slice(fcIndex + 1).filter((t) => t.name === "read").length : 0;
  const errored = result.tools.filter((t) => t.isError);
  return {
    checks,
    summary: {
      toolCalls: result.tools.length,
      fastContextCalls: fcCalls.length,
      findSymbolsCalls: fsCalls.length,
      readsAfterFc: readsAfter,
      toolErrors: errored.map((t) => t.name),
      sequence: result.tools.map((t) => t.name + (t.isError ? "✗" : "")).join(" → "),
      wallMs: result.wallMs,
      timeout: !!result.timeout,
      error: result.error,
    },
  };
}

const args = process.argv.slice(2);
const model = args.find((a) => !a.startsWith("--")) || "deepseek/deepseek-v4-flash";
const only = args.find((a) => a.startsWith("--case="))?.split("=")[1]?.split(",");
const cases = CASES.filter((c) => !only || only.includes(c.id));

const reportPath = join(HERE, "fast-context-dsv4.report.json");
let report = { model, ranAt: new Date().toISOString(), cases: [] };
try {
  const prev = JSON.parse(readFileSync(reportPath, "utf8"));
  if (prev.model === model && Array.isArray(prev.cases)) report = { ...prev, ranAt: new Date().toISOString() };
} catch {}
report.cases = report.cases.filter((c) => !cases.some((t) => t.id === c.id));
for (const testCase of cases) {
  process.stderr.write(`\n=== ${testCase.id} (${testCase.kind}) ===\n`);
  const result = await runCase(model, testCase);
  const scored = score(testCase, result);
  const passed = scored.checks.filter((c) => c.pass).length;
  report.cases.push({ id: testCase.id, kind: testCase.kind, prompt: testCase.prompt, pass: `${passed}/${scored.checks.length}`, checks: scored.checks, ...scored.summary, tools: result.tools.map((t) => ({ name: t.name, args: t.args, isError: t.isError })), finalTextTail: result.finalText.slice(-400) });
  console.log(`\n${testCase.id} [${testCase.kind}] pass ${passed}/${scored.checks.length} wall=${(result.wallMs / 1000).toFixed(1)}s tools=${scored.summary.toolCalls}`);
  console.log(`  sequence: ${scored.summary.sequence || "(无工具调用)"}`);
  for (const c of scored.checks) console.log(`  ${c.pass ? "✓" : "✗"} ${c.name}${c.detail ? ` — ${c.detail}` : ""}`);
}
writeFileSync(reportPath, JSON.stringify(report, null, 2));
const totalChecks = report.cases.flatMap((c) => c.checks);
console.log(`\n== 总计 ${totalChecks.filter((c) => c.pass).length}/${totalChecks.length} 项通过，报告: scripts/fast-context-dsv4.report.json ==`);
