// 真实软件工程会话端到端 A/B：A 移除 fast_context span 契约，B 保留。
// 每个 arm 在独立 git worktree 运行 Vega，采集耗时、工具、token、diff、测试与真实编辑标签。
// 默认只跑可安全自动验证的小任务；--limit N。成本较高。
import { execFile, spawn } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync, statSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { basename, isAbsolute, join, relative, resolve } from "node:path";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";
import { createAlkaidAgent } from "./alkaid-core.mjs";
import { loadAlkaidConfig, resolveAlkaidModel } from "./alkaid-config.mjs";

const exec = promisify(execFile);
const argv = process.argv.slice(2);
const opt = (name, fallback) => { const i = argv.indexOf(name); return i >= 0 ? argv[i + 1] : fallback; };
const limit = Math.max(1, Number(opt("--limit", "4")) || 4);
const out = opt("--out", "docs/fast-context-session-ab-report.md");
const threads = String(opt("--dir", join(homedir(), ".nova", "threads"))).replace(/^~(?=[\\/]|$)/, homedir());
const selection = "opencode/deepseek-v4-flash/variant/high";
const timeoutMs = Math.max(30_000, Number(opt("--timeout-ms", "180000")) || 180_000);
const maxToolCalls = Math.max(5, Number(opt("--max-tools", "20")) || 20);
const parallel = Math.max(1, Number(opt("--parallel", "6")) || 6);
const key = (item) => item.id ?? item.ts ?? 0;

function classify(item) {
  const head = String(item.title || "").split("·")[0];
  if (/\bfast_context\b/.test(head)) return "fast_context";
  if (/\bfind_symbols\b/.test(head)) return "find_symbols";
  if (/\bread\b/.test(head)) return "read";
  if (/\b(edit|write)\b/.test(head) || /^修改 /.test(item.title || "")) return "edit";
  if (/\b(shell|bash)\b/.test(head)) return "bash";
  return "other";
}
function editPaths(item, cwd) {
  const list = Array.isArray(item.rawInput?.files) ? item.rawInput.files.map((x) => x.path) : [item.rawInput?.path].filter(Boolean);
  return list.map((path) => {
    if (!isAbsolute(path)) return String(path).replaceAll("\\", "/");
    const value = relative(cwd, path);
    return (value.startsWith("..") ? path : value).replaceAll("\\", "/");
  });
}
function cases(thread) {
  const result = []; let task = "", turn = [];
  const flush = () => {
    const edits = turn.filter((x) => classify(x) === "edit");
    const targets = [...new Set(edits.flatMap((x) => editPaths(x, thread.cwd || "")))];
    const calls = turn.filter((x) => classify(x) === "fast_context");
    const validation = turn.filter((x) => classify(x) === "bash").map((x) => String(x.title || "").split("·").slice(1).join("·").trim())
      .filter((x) => /(?:npm|node|cargo|pnpm|yarn).*(?:test|check|build)|git diff --check/i.test(x));
    if (task.trim().length >= 4 && task.length <= 1200 && targets.length >= 1 && targets.length <= 4 && calls.length) {
      result.push({ task, cwd: thread.cwd, targets, validation: validation.at(-1) || "" });
    }
    turn = [];
  };
  for (const item of [...(thread.items || [])].sort((a, b) => key(a) - key(b))) {
    if (item.type === "user") { flush(); task = String(item.text || ""); }
    else if (item.type === "tool") turn.push(item);
  }
  flush(); return result;
}
function loadSamples() {
  const rows = [];
  for (const file of readdirSync(threads).filter((x) => x.endsWith(".json")).sort((a, b) => statSync(join(threads, b)).mtimeMs - statSync(join(threads, a)).mtimeMs)) {
    try { rows.push(...cases(JSON.parse(readFileSync(join(threads, file), "utf8")))); } catch {}
    if (rows.length >= limit * 8) break;
  }
  const selected = [], seen = new Set();
  for (const row of rows) {
    if (!row.cwd || !existsSync(join(row.cwd, ".git"))) continue;
    const id = `${row.cwd}\n${row.task}`; if (seen.has(id)) continue;
    seen.add(id); selected.push(row); if (selected.length >= limit) break;
  }
  return selected;
}
function withoutSpan(value) {
  return String(value).replace(/ complete=(?:true|false)/g, "").replace(/ span=\d+-\d+/g, "")
    .replace(/ editUnitSpan=\d+-\d+/g, "").replace(/ symbolSpan=\d+-\d+/g, "")
    .replace(/ boundary=(?:ast|file|heuristic)/g, "").replace(/\n## OMITTED \(完整单元未展开\)[\s\S]*?(?=\n## |$)/g, "")
    .replace(/每段含 inclusive endLine 与边界来源。/g, "")
    .replace(/完整调用单元 span; 确需正文按 path:start-end 精确补读/g, "仅行; 确需函数体按 path:ln 补读")
    .replace(/仅签名但含完整 span/g, "仅签名").replace(/^(\S+):(\d+)-\d+ hitLine=\d+ /gm, "$1:$2 ")
    .replace(/^(\S+):(\d+)-\d+ /gm, "$1:$2 ");
}
async function command(cwd, file, args, timeout = 120_000) {
  try { const value = await exec(file, args, { cwd, timeout, windowsHide: true, maxBuffer: 8 * 1024 * 1024 }); return { ok: true, text: `${value.stdout}\n${value.stderr}`.trim() }; }
  catch (error) { return { ok: false, text: `${error.stdout || ""}\n${error.stderr || error.message}`.trim().slice(-4000) }; }
}
async function worktree(root, path) {
  const result = await command(root, "git", ["worktree", "add", "--detach", path, "HEAD"]);
  if (!result.ok) throw new Error(result.text);
}
function sumUsage(total, usage = {}) {
  for (const field of ["input", "output", "cacheRead", "cacheWrite", "totalTokens", "inputTokens", "outputTokens"]) total[field] = (total[field] || 0) + (Number(usage[field]) || 0);
}
async function runArm(sample, arm, cwd, resolved) {
  const started = performance.now(); let firstToken, firstTool, finalText = ""; const tools = []; const usage = {}; let assistantMessages = 0;
  const runtime = await createAlkaidAgent({ cwd, model: resolved.model, apiKey: resolved.apiKey, thinkingLevel: "high", sessionId: `ctx-e2e-${arm}-${Date.now()}`,
    fastContextTransform: arm === "A" ? withoutSpan : undefined,
    systemPrompt: `这是受控 A/B 评测。请自主完成任务并实际修改文件；执行成本最低的有效验证。不要询问用户，不要提交 git。最多使用 ${maxToolCalls} 次工具：第 1 次优先 fast_context，第 10 次工具前必须开始 edit/write；禁止重复检索同一目标。` });
  runtime.agent.subscribe((event) => {
    const elapsed = Math.round(performance.now() - started);
    if (event.type === "message_update" && event.assistantMessageEvent.type === "text_delta") { firstToken ??= elapsed; finalText += event.assistantMessageEvent.delta; }
    else if (event.type === "tool_execution_start") {
      firstTool ??= elapsed;
      tools.push({ name: event.toolName, start: elapsed, end: null, error: false });
      if (tools.length >= maxToolCalls) runtime.agent.abort();
    }
    else if (event.type === "tool_execution_end") { const row = [...tools].reverse().find((x) => x.name === event.toolName && x.end == null); if (row) { row.end = elapsed; row.error = !!event.isError; } }
    else if (event.type === "message_end" && event.message.role === "assistant") { assistantMessages++; sumUsage(usage, event.message.usage); }
  });
  let error = "", timer;
  try {
    await Promise.race([
      runtime.agent.prompt(sample.task),
      new Promise((_, reject) => { timer = setTimeout(() => { runtime.agent.abort(); reject(new Error("session timeout")); }, timeoutMs); }),
    ]);
  } catch (value) { error = String(value?.message || value); }
  finally { clearTimeout(timer); await runtime.close(); }
  const wallMs = Math.round(performance.now() - started);
  const status = await command(cwd, "git", ["status", "--porcelain=v1"]);
  const changed = status.text.split(/\r?\n/).filter(Boolean).map((line) => line.slice(3).replaceAll("\\", "/"));
  const diff = await command(cwd, "git", ["diff", "--numstat"]);
  const diffLines = diff.text.split(/\r?\n/).filter(Boolean).reduce((n, line) => { const [a, d] = line.split("\t"); return n + (Number(a) || 0) + (Number(d) || 0); }, 0);
  let validation = { ok: null, command: "", text: "" };
  if (sample.validation && !/[;&|]/.test(sample.validation)) {
    const parts = sample.validation.match(/(?:[^\s"]+|"[^"]*")+/g) || [];
    if (parts.length) validation = { ...(await command(cwd, parts[0].replace(/^npm$/, "npm.cmd"), parts.slice(1).map((x) => x.replace(/^"|"$/g, "")), 300_000)), command: sample.validation };
  }
  const targetHit = sample.targets.filter((x) => changed.includes(x)).length;
  return { wallMs, firstTokenMs: firstToken ?? null, firstToolMs: firstTool ?? null, assistantMessages, usage, tools, toolCounts: Object.fromEntries([...new Set(tools.map((x) => x.name))].map((name) => [name, tools.filter((x) => x.name === name).length])), toolErrors: tools.filter((x) => x.error).length,
    changed, diffLines, targetHit, targetRecall: sample.targets.length ? targetHit / sample.targets.length : 0, exactTargets: targetHit === sample.targets.length, extraFiles: changed.filter((x) => !sample.targets.includes(x)).length,
    validation: { command: validation.command, ok: validation.ok, tail: validation.text.slice(-800) }, error, finalText: finalText.slice(-1200) };
}
const mean = (xs) => xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : 0;
const pct = (x) => `${(100 * x).toFixed(1)}%`;
function aggregate(rows, arm) {
  const values = rows.map((x) => x[arm]), calls = values.flatMap((x) => x.tools);
  return { wallMs: mean(values.map((x) => x.wallMs)), firstToolMs: mean(values.map((x) => x.firstToolMs || x.wallMs)), tools: mean(values.map((x) => x.tools.length)), reads: mean(values.map((x) => (x.toolCounts.read || 0))), fastContext: mean(values.map((x) => x.toolCounts.fast_context || 0)), edits: mean(values.map((x) => (x.toolCounts.edit || 0) + (x.toolCounts.write || 0))), bash: mean(values.map((x) => x.toolCounts.bash || 0)), errors: calls.filter((x) => x.error).length,
    recall: mean(values.map((x) => x.targetRecall)), exact: mean(values.map((x) => Number(x.exactTargets))), extra: mean(values.map((x) => x.extraFiles)), diffLines: mean(values.map((x) => x.diffLines)), validation: mean(values.filter((x) => x.validation.ok != null).map((x) => Number(x.validation.ok))), output: values.reduce((n, x) => n + (x.usage.output || x.usage.outputTokens || 0), 0), input: values.reduce((n, x) => n + (x.usage.input || x.usage.inputTokens || 0), 0) };
}
async function main() {
  const samples = loadSamples(); if (!samples.length) throw new Error("没有可评估真实任务");
  const config = await loadAlkaidConfig(); const resolved = resolveAlkaidModel(config, selection);
  // git worktree 元数据写入需要串行；准备完成后，所有 A/B 会话按限流并行。
  const jobs = [];
  for (let i = 0; i < samples.length; i++) {
    const temp = mkdtempSync(join(tmpdir(), "nova-ctx-e2e-"));
    for (const arm of ["A", "B"]) {
      const cwd = join(temp, arm); await worktree(samples[i].cwd, cwd);
      jobs.push({ i, arm, cwd, temp, sample: samples[i] });
    }
  }
  const results = Array(samples.length).fill(null).map(() => ({}));
  let cursor = 0;
  const worker = async () => {
    while (cursor < jobs.length) {
      const job = jobs[cursor++];
      const result = await runArm(job.sample, job.arm, job.cwd, resolved);
      results[job.i][job.arm] = result;
      console.log(`[${job.i + 1}/${samples.length}] ${job.arm} ${(result.wallMs / 1000).toFixed(1)}s tools=${result.tools.length} recall=${pct(result.targetRecall)}`);
      writeFileSync(`${out}.checkpoint.json`, JSON.stringify({ samples, results }, null, 2));
    }
  };
  try {
    await Promise.all(Array.from({ length: Math.min(parallel, jobs.length) }, worker));
  } finally {
    for (const job of jobs) await command(job.sample.cwd, "git", ["worktree", "remove", "--force", job.cwd]);
    for (const temp of new Set(jobs.map((job) => job.temp))) rmSync(temp, { recursive: true, force: true });
  }
  const rows = samples.map((sample, i) => ({ task: sample.task, targets: sample.targets, validationCommand: sample.validation, ...results[i] }));
  const A = aggregate(rows, "A"), B = aggregate(rows, "B"), seconds = (x) => `${(x / 1000).toFixed(1)}s`, delta = (b, a) => `${b >= a ? "+" : ""}${(b - a).toFixed(1)}`;
  const report = `# fast_context 完整会话端到端 A/B 报告\n\n- 日期：${new Date().toISOString()}\n- 模型：\`${selection}\`\n- 样本：${rows.length} 个来自 Nova 真实会话的软件工程任务\n- 隔离：每个 arm 使用独立 detached git worktree；不修改原工作区、不提交\n- A：当前 fast_context 正文移除 span/complete/boundary/OMITTED 契约；B：完整 span 契约\n\n## 总览\n\n| 指标（每会话均值） | A | B | B-A |\n|---|---:|---:|---:|\n| 完整会话耗时 | ${seconds(A.wallMs)} | ${seconds(B.wallMs)} | ${seconds(B.wallMs - A.wallMs)} |\n| 首次工具调用 | ${seconds(A.firstToolMs)} | ${seconds(B.firstToolMs)} | ${seconds(B.firstToolMs - A.firstToolMs)} |\n| 工具调用总数 | ${A.tools.toFixed(1)} | ${B.tools.toFixed(1)} | ${delta(B.tools, A.tools)} |\n| read | ${A.reads.toFixed(1)} | ${B.reads.toFixed(1)} | ${delta(B.reads, A.reads)} |\n| fast_context | ${A.fastContext.toFixed(1)} | ${B.fastContext.toFixed(1)} | ${delta(B.fastContext, A.fastContext)} |\n| edit/write | ${A.edits.toFixed(1)} | ${B.edits.toFixed(1)} | ${delta(B.edits, A.edits)} |\n| bash/验证 | ${A.bash.toFixed(1)} | ${B.bash.toFixed(1)} | ${delta(B.bash, A.bash)} |\n| 工具错误总数 | ${A.errors} | ${B.errors} | ${B.errors - A.errors} |\n| 真实编辑文件召回 | ${pct(A.recall)} | ${pct(B.recall)} | ${(100 * (B.recall - A.recall)).toFixed(1)} pp |\n| 全目标命中率 | ${pct(A.exact)} | ${pct(B.exact)} | ${(100 * (B.exact - A.exact)).toFixed(1)} pp |\n| 额外修改文件 | ${A.extra.toFixed(1)} | ${B.extra.toFixed(1)} | ${delta(B.extra, A.extra)} |\n| diff 行数 | ${A.diffLines.toFixed(1)} | ${B.diffLines.toFixed(1)} | ${delta(B.diffLines, A.diffLines)} |\n| 可复用验证通过率 | ${pct(A.validation)} | ${pct(B.validation)} | ${(100 * (B.validation - A.validation)).toFixed(1)} pp |\n| input tokens 总计 | ${A.input} | ${B.input} | ${B.input - A.input} |\n| output tokens 总计 | ${A.output} | ${B.output} | ${B.output - A.output} |\n\n## 逐任务\n\n| # | 任务 | A/B 耗时 | A/B 工具 | A/B read | A/B 召回 | A/B 验证 | A/B 额外文件 |\n|---:|---|---:|---:|---:|---:|---:|---:|\n${rows.map((r, i) => `| ${i + 1} | ${r.task.replaceAll("|", "\\|").replaceAll("\n", " ").slice(0, 72)} | ${seconds(r.A.wallMs)}/${seconds(r.B.wallMs)} | ${r.A.tools.length}/${r.B.tools.length} | ${r.A.toolCounts.read || 0}/${r.B.toolCounts.read || 0} | ${pct(r.A.targetRecall)}/${pct(r.B.targetRecall)} | ${String(r.A.validation.ok)}/${String(r.B.validation.ok)} | ${r.A.extraFiles}/${r.B.extraFiles} |`).join("\n")}\n\n## 解释\n\n- “效果”同时看真实编辑文件召回、额外文件、diff 规模和验证结果；不以模型自述作为成功证据。\n- 真实会话的后续编辑文件是弱标签：原会话可能包含追问或人工纠偏，因此不能等同唯一正确答案。\n- 若验证命令为空，验证率不计该样本；完整原始指标见同名 JSON。\n`;
  writeFileSync(out, report); writeFileSync(`${out}.json`, JSON.stringify({ selection, rows, A, B }, null, 2)); console.log(report);
}
const isMain = process.argv[1] && realpathSync(process.argv[1]) === realpathSync(fileURLToPath(import.meta.url)); if (isMain) await main();
