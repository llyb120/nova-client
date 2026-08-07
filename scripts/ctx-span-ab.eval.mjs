// 用真实 Nova 会话对 fast_context 的 span 输出做模型 A/B。
// A = 当前回放输出移除 span 元数据（旧契约模拟）；B = 当前回放的完整 span 输出。
// 两臂正文完全相同，只隔离评估 span/complete/boundary/omitted 契约，避免代码版本漂移混淆。
// 凭据只从环境变量 OPENCODE_API_KEY，或本机 Vega config.jsonc 读取，绝不写入报告。
// 用法: node scripts/ctx-span-ab.eval.mjs [--limit 12] [--dir ~/.nova/threads] [--out docs/fast-context-span-ab-report.md]

import { existsSync, readFileSync, readdirSync, realpathSync, statSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { isAbsolute, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { callNapiTool } from "./nova-napi-tools.mjs";

const argv = process.argv.slice(2);
const option = (name, fallback) => {
  const i = argv.indexOf(name);
  return i >= 0 ? argv[i + 1] : fallback;
};
const expandHome = (value) => value.replace(/^~(?=[\\/]|$)/, homedir());
const threadDir = expandHome(option("--dir", join(homedir(), ".nova", "threads")));
const limit = Math.max(1, Number(option("--limit", "12")) || 12);
const outPath = option("--out", "docs/fast-context-span-ab-report.md");
const endpoint = "https://opencode.ai/zen/go/v1/chat/completions";
const model = "deepseek-v4-flash";

function strings(value, out = []) {
  if (typeof value === "string") out.push(value);
  else if (Array.isArray(value)) value.forEach((item) => strings(item, out));
  else if (value && typeof value === "object") Object.values(value).forEach((item) => strings(item, out));
  return out;
}
function outputOf(item) {
  return [strings(item.rawOutput).join("\n"), strings(item.content).join("\n")].filter(Boolean).join("\n");
}
function tool(item) {
  const head = String(item.title || "").split("·")[0];
  if (/\bfast_context\b/.test(head)) return "fast_context";
  if (/\b(edit|write)\b/.test(head) || /^修改 /.test(item.title || "")) return "edit";
  return "other";
}
function editPaths(item, cwd) {
  const paths = Array.isArray(item.rawInput?.files)
    ? item.rawInput.files.map((file) => file.path)
    : [item.rawInput?.path].filter(Boolean);
  return paths.map((path) => {
    if (!isAbsolute(path)) return path.replaceAll("\\", "/");
    const value = relative(cwd, path);
    return (value.startsWith("..") ? path : value).replaceAll("\\", "/");
  });
}
const key = (item) => item.id ?? item.ts ?? 0;

function casesFromThread(thread) {
  const items = [...(thread.items || [])].sort((a, b) => key(a) - key(b));
  const result = [];
  let task = "";
  let turn = [];
  const flush = () => {
    const calls = turn.filter((item) => tool(item) === "fast_context");
    for (const call of calls) {
      const targets = new Set();
      for (const item of turn) {
        if (tool(item) === "edit" && key(item) > key(call)) {
          editPaths(item, thread.cwd || "").forEach((path) => targets.add(path));
        }
      }
      const recorded = outputOf(call);
      if (targets.size && recorded && call.rawInput && thread.cwd && existsSync(thread.cwd)) {
        result.push({ task, cwd: thread.cwd, input: call.rawInput, recorded, targets: [...targets] });
      }
    }
    turn = [];
  };
  for (const item of items) {
    if (item.type === "user") {
      flush();
      task = String(item.text || "").slice(0, 500);
    } else if (item.type === "tool") turn.push(item);
  }
  flush();
  return result;
}

function loadApiKey() {
  if (process.env.OPENCODE_API_KEY) return process.env.OPENCODE_API_KEY;
  for (const root of [process.env.NOVA_DATA_DIR, join(homedir(), ".nova"), join(homedir(), ".novadev")].filter(Boolean)) {
    const path = join(root, "alkaid", "config.jsonc");
    if (!existsSync(path)) continue;
    const text = readFileSync(path, "utf8");
    const match = text.match(/"opencode"\s*:\s*\{[\s\S]*?"apiKey"\s*:\s*"([^"]+)"/);
    if (match) return match[1];
  }
  throw new Error("缺少 OPENCODE_API_KEY，且本机 Vega 配置中未找到 opencode apiKey");
}

function withoutSpanContract(context) {
  return context
    .replace(/ complete=(?:true|false)/g, "")
    .replace(/ span=\d+-\d+/g, "")
    .replace(/ editUnitSpan=\d+-\d+/g, "")
    .replace(/ symbolSpan=\d+-\d+/g, "")
    .replace(/ boundary=(?:ast|file|heuristic)/g, "")
    .replace(/\n## OMITTED \(完整单元未展开\)[\s\S]*?(?=\n## |$)/g, "")
    .replace(/每段含 inclusive endLine 与边界来源。/g, "")
    .replace(/完整调用单元 span; 确需正文按 path:start-end 精确补读/g, "仅行; 确需函数体按 path:ln 补读")
    .replace(/仅签名但含完整 span/g, "仅签名")
    .replace(/^(\S+):(\d+)-\d+ hitLine=\d+ /gm, "$1:$2 ")
    .replace(/^(\S+):(\d+)-\d+ /gm, "$1:$2 ");
}

async function judge(apiKey, sample, context, arm) {
  const prompt = `你是代码代理。下面是一个真实软件工程任务，以及 fast_context 返回的仓库上下文。\n请只输出 JSON：{"editFiles":["仓库相对路径"],"needsAdditionalRead":true|false,"reason":"不超过80字"}。\neditFiles 预测完成任务最可能需要修改的文件；上下文若已给出完整可编辑单元和明确边界，则不要因谨慎而要求 read。\n\n任务：${sample.task}\n\nfast_context：\n${context}`;
  const started = performance.now();
  const response = await fetch(endpoint, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${apiKey}` },
    body: JSON.stringify({
      model,
      messages: [{ role: "user", content: prompt }],
      temperature: 0,
      // reasoning 模型会把思考 token 计入上限；留足空间确保最终 JSON 不被截断。
      max_tokens: 6000,
      reasoning_effort: "high",
      response_format: { type: "json_object" },
    }),
  });
  if (!response.ok) throw new Error(`${arm} HTTP ${response.status}: ${(await response.text()).slice(0, 300)}`);
  const payload = await response.json();
  const text = payload.choices?.[0]?.message?.content || "{}";
  const parsed = JSON.parse(text.match(/\{[\s\S]*\}/)?.[0] || "{}");
  return {
    files: Array.isArray(parsed.editFiles) ? parsed.editFiles.map((path) => String(path).replaceAll("\\", "/")) : [],
    needsRead: parsed.needsAdditionalRead === true,
    reason: String(parsed.reason || ""),
    latencyMs: Math.round(performance.now() - started),
    usage: payload.usage || {},
  };
}

const ratio = (n, d) => d ? n / d : 0;
const pct = (value) => `${(value * 100).toFixed(1)}%`;
function score(rows, arm) {
  let hit = 0, total = 0, exact = 0, reads = 0, latency = 0, outputTokens = 0;
  for (const row of rows) {
    const predicted = new Set(row[arm].files);
    const matched = row.targets.filter((path) => predicted.has(path)).length;
    hit += matched; total += row.targets.length;
    exact += matched === row.targets.length ? 1 : 0;
    reads += row[arm].needsRead ? 1 : 0;
    latency += row[arm].latencyMs;
    outputTokens += row[arm].usage.completion_tokens || 0;
  }
  return { recall: ratio(hit, total), exact: ratio(exact, rows.length), readRate: ratio(reads, rows.length), latency: ratio(latency, rows.length), outputTokens };
}

async function main() {
  const files = readdirSync(threadDir).filter((file) => file.endsWith(".json"))
    .sort((a, b) => statSync(join(threadDir, b)).mtimeMs - statSync(join(threadDir, a)).mtimeMs);
  const samples = [];
  for (const file of files) {
    try {
      samples.push(...casesFromThread(JSON.parse(readFileSync(join(threadDir, file), "utf8"))));
    } catch { /* 损坏或不兼容线程跳过 */ }
    if (samples.length >= limit * 3) break;
  }
  // 确定性抽样：优先不同 cwd/task，避免同一长会话垄断样本。
  const selected = [];
  const seen = new Set();
  for (const sample of samples) {
    const id = `${sample.cwd}\n${sample.task}`;
    if (seen.has(id)) continue;
    seen.add(id); selected.push(sample);
    if (selected.length >= limit) break;
  }
  const apiKey = loadApiKey();
  const rows = [];
  for (let index = 0; index < selected.length; index += 1) {
    const sample = selected[index];
    const current = await callNapiTool("fast_context", sample.cwd, sample.input);
    const baseline = withoutSpanContract(current);
    // 交替调用顺序，降低 provider 热缓存和时间漂移偏差。
    const order = index % 2 ? [["B", current], ["A", baseline]] : [["A", baseline], ["B", current]];
    const judged = {};
    for (const [arm, context] of order) judged[arm] = await judge(apiKey, sample, context, arm);
    rows.push({ task: sample.task, cwd: sample.cwd, targets: sample.targets, A: judged.A, B: judged.B,
      recordedChars: baseline.length, currentChars: current.length });
    console.log(`[${index + 1}/${selected.length}] A read=${judged.A.needsRead} B read=${judged.B.needsRead} ${sample.task.slice(0, 50)}`);
  }
  const A = score(rows, "A"), B = score(rows, "B");
  const report = `# fast_context 完整 span 优化 A/B 报告\n\n` +
`- 日期：${new Date().toISOString()}\n- 模型：\`opencode/${model}\`（reasoningEffort=high，temperature=0）\n- 数据：本机 Nova 真实会话 rawInput 与后续实际编辑文件；A/B 均由当前 native 回放，A 仅移除 span 契约，B 保留 span 契约\n- 样本：${rows.length} 次可评估 fast_context 调用；真实标签为调用后实际编辑文件\n- 安全：报告不包含 API key 或完整会话正文\n\n` +
`## 汇总\n\n| 指标 | A 旧输出 | B span 输出 | 变化 |\n|---|---:|---:|---:|\n` +
`| 实际编辑文件召回率 | ${pct(A.recall)} | ${pct(B.recall)} | ${(100 * (B.recall - A.recall)).toFixed(1)} pp |\n` +
`| 全目标命中率 | ${pct(A.exact)} | ${pct(B.exact)} | ${(100 * (B.exact - A.exact)).toFixed(1)} pp |\n` +
`| 模型要求额外 read | ${pct(A.readRate)} | ${pct(B.readRate)} | ${(100 * (B.readRate - A.readRate)).toFixed(1)} pp |\n` +
`| 平均模型延迟 | ${A.latency.toFixed(0)} ms | ${B.latency.toFixed(0)} ms | ${(B.latency - A.latency).toFixed(0)} ms |\n` +
`| completion tokens | ${A.outputTokens} | ${B.outputTokens} | ${B.outputTokens - A.outputTokens} |\n\n` +
`## 逐样本\n\n| # | 任务（截断） | 真实编辑数 | A 命中 | B 命中 | A/B 额外 read | 输出字符 A→B |\n|---:|---|---:|---:|---:|---|---:|\n` +
rows.map((row, index) => {
  const hits = (arm) => row.targets.filter((path) => row[arm].files.includes(path)).length;
  return `| ${index + 1} | ${row.task.replaceAll("|", "\\|").replaceAll("\n", " ").slice(0, 70)} | ${row.targets.length} | ${hits("A")} | ${hits("B")} | ${row.A.needsRead}/${row.B.needsRead} | ${row.recordedChars}→${row.currentChars} |`;
}).join("\n") +
`\n\n## 判定\n\n${B.readRate < A.readRate ? "完整 span 降低了模型补读倾向。" : B.readRate === A.readRate ? "本样本中补读倾向持平；需扩大样本确认。" : "完整 span 未降低补读倾向，需检查输出噪声与提示契约。"} ` +
`${B.recall >= A.recall ? "编辑文件召回未回归。" : "编辑文件召回出现回归，应检查具体样本后再发布。"}\n`;
  writeFileSync(outPath, report, "utf8");
  writeFileSync(`${outPath}.json`, JSON.stringify({ model, rows, A, B }, null, 2), "utf8");
  console.log(report);
}

const isMain = process.argv[1] && realpathSync(process.argv[1]) === realpathSync(fileURLToPath(import.meta.url));
if (isMain) await main();
