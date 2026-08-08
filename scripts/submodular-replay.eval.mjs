#!/usr/bin/env node
// 真实会话 replay A/B：从 ~/.nova/alkaid/sessions 提取真实 fast_context 调用参数，
// 直接调 napi（不走模型，零 RTT），对同一组参数跑两臂对比打包质量。
// A 臂 = 全部新特性关闭（NOVA_CTX_SUBMODULAR=0 MERGED_SEARCH=0 DEFS_SUGGEST=0 PREFETCH=0 SPECULATIVE=0）
// B 臂 = 全部新特性开启（默认）
// 用法：node scripts/submodular-replay.eval.mjs [--limit N] [--arm A|B|both]
import { readFileSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join, resolve } from "node:path";

const REPO = resolve(import.meta.dirname, "..");
const NAPI_PATH = join(REPO, "src-tauri", "resources", "nova-tools-napi.node");
const REPORT_JSON = join(REPO, "scripts", "submodular-replay.report.json");
const REPORT_MD = join(REPO, "scripts", "submodular-replay.report.md");

const args = process.argv.slice(2);
const limitIndex = args.indexOf("--limit");
const LIMIT = limitIndex >= 0 ? Number(args[limitIndex + 1]) : 0;
const armIndex = args.indexOf("--arm");
const ARM_FILTER = armIndex >= 0 ? args[armIndex + 1] : "both";

process.env.NOVA_TOOLS_NAPI_PATH = NAPI_PATH;
const { callNapiTool } = await import("./nova-napi-tools.mjs");

// ---------- 提取真实 fast_context 调用参数 ----------
function extractRealCalls(maxSessions = 50) {
  const dir = join(homedir(), ".nova", "alkaid", "sessions");
  let files;
  try {
    files = readdirSync(dir)
      .filter((name) => name.endsWith(".slim.json"))
      .map((name) => join(dir, name))
      .sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs)
      .slice(0, maxSessions);
  } catch {
    return [];
  }
  const calls = [];
  const seen = new Set();
  for (const path of files) {
    let data;
    try { data = JSON.parse(readFileSync(path, "utf8")); } catch { continue; }
    for (const msg of data.fullMessages ?? []) {
      for (const part of msg.content ?? []) {
        if (part?.type !== "toolCall" || part.name !== "fast_context") continue;
        const a = part.arguments ?? {};
        const keywords = (Array.isArray(a.keywords) ? a.keywords : []).map(String).filter(Boolean);
        if (!keywords.length) continue;
        const dedupKey = keywords.join(",");
        if (seen.has(dedupKey)) continue;
        seen.add(dedupKey);
        calls.push({
          session: path.split("/").pop(),
          params: {
            keywords,
            task: String(a.task ?? ""),
            files: Array.isArray(a.files) ? a.files : [],
            budget: a.budget,
            maxBytes: a.maxBytes,
            coupling: !!a.coupling,
          },
        });
      }
    }
  }
  return calls;
}

// ---------- 打包质量指标 ----------
// 从 fast_context 输出文本中提取结构化指标。
function analyzeOutput(text) {
  const lines = text.split("\n");
  const files = new Set();
  const sections = new Set();
  let blockCount = 0;
  let speculativeCount = 0;
  let sigCount = 0;
  let impactCount = 0;
  for (const line of lines) {
    if (line.startsWith("### ")) {
      const match = line.match(/^### (.+?) \(/);
      if (match) files.add(match[1]);
    }
    if (line.startsWith("## ")) sections.add(line.slice(3).split(" ")[0].split("(")[0].trim());
    if (line.startsWith("@@ ")) blockCount += 1;
    if (line.startsWith("# CTX MISS")) return { miss: true, files: 0, blocks: 0, sections: [], bytes: text.length };
  }
  // 按段统计
  let currentSection = "";
  for (const line of lines) {
    if (line.startsWith("## SPECULATIVE")) { currentSection = "speculative"; continue; }
    if (line.startsWith("## SIG")) { currentSection = "sig"; continue; }
    if (line.startsWith("## IMPACT")) { currentSection = "impact"; continue; }
    if (line.startsWith("## ")) { currentSection = ""; continue; }
    if (currentSection === "speculative" && line.trim() && !line.startsWith("#")) speculativeCount += 1;
    if (currentSection === "sig" && line.trim() && !line.startsWith("#")) sigCount += 1;
    if (currentSection === "impact" && line.trim() && !line.startsWith("#")) impactCount += 1;
  }
  // 文件覆盖熵：打包的文件数越多越好（多样性）
  return {
    miss: false,
    files: files.size,
    blocks: blockCount,
    sections: [...sections],
    bytes: text.length,
    speculativeCount,
    sigCount,
    impactCount,
  };
}

// ---------- 单臂 replay ----------
async function replayArm(calls, arm) {
  const envKey = {
    A: { NOVA_CTX_SUBMODULAR: "0", NOVA_CTX_MERGED_SEARCH: "0", NOVA_CTX_DEFS_SUGGEST: "0", NOVA_CTX_PREFETCH: "0", NOVA_CTX_SPECULATIVE: "0" },
    B: {}, // 全部默认开启
  }[arm];
  // 设置环境变量
  const saved = {};
  for (const [key, value] of Object.entries(envKey)) {
    saved[key] = process.env[key];
    process.env[key] = value;
  }
  // 清理被删除的变量
  if (arm === "B") {
    for (const key of ["NOVA_CTX_SUBMODULAR", "NOVA_CTX_MERGED_SEARCH", "NOVA_CTX_DEFS_SUGGEST", "NOVA_CTX_PREFETCH", "NOVA_CTX_SPECULATIVE"]) {
      saved[key] = saved[key] ?? process.env[key];
      delete process.env[key];
    }
  }

  const results = [];
  for (const call of calls) {
    const startedAt = Date.now();
    let output;
    let error;
    try {
      output = await callNapiTool("fast_context", REPO, call.params);
    } catch (e) {
      error = String(e?.message ?? e);
    }
    const wallMs = Date.now() - startedAt;
    const analysis = output ? analyzeOutput(output) : { miss: true, files: 0, blocks: 0, sections: [], bytes: 0 };
    results.push({
      session: call.session,
      keywords: call.params.keywords,
      wallMs,
      error,
      ...analysis,
    });
  }

  // 恢复环境变量
  for (const [key, value] of Object.entries(saved)) {
    if (value === undefined) delete process.env[key];
    else process.env[key] = value;
  }
  return results;
}

// ---------- 汇总 ----------
function summarize(results) {
  const ok = results.filter((r) => !r.error && !r.miss);
  const miss = results.filter((r) => r.miss);
  const errors = results.filter((r) => r.error);
  return {
    total: results.length,
    ok: ok.length,
    miss: miss.length,
    errors: errors.length,
    totalWallMs: results.reduce((s, r) => s + r.wallMs, 0),
    avgWallMs: ok.length ? Math.round(ok.reduce((s, r) => s + r.wallMs, 0) / ok.length) : 0,
    avgFiles: ok.length ? +(ok.reduce((s, r) => s + r.files, 0) / ok.length).toFixed(1) : 0,
    avgBlocks: ok.length ? +(ok.reduce((s, r) => s + r.blocks, 0) / ok.length).toFixed(1) : 0,
    avgBytes: ok.length ? Math.round(ok.reduce((s, r) => s + r.bytes, 0) / ok.length) : 0,
    avgSpeculative: ok.length ? +(ok.reduce((s, r) => s + (r.speculativeCount ?? 0), 0) / ok.length).toFixed(1) : 0,
    avgSig: ok.length ? +(ok.reduce((s, r) => s + (r.sigCount ?? 0), 0) / ok.length).toFixed(1) : 0,
    avgImpact: ok.length ? +(ok.reduce((s, r) => s + (r.impactCount ?? 0), 0) / ok.length).toFixed(1) : 0,
  };
}

// ---------- 主流程 ----------
const allCalls = extractRealCalls();
const calls = LIMIT > 0 ? allCalls.slice(0, LIMIT) : allCalls;
console.log(`提取到 ${allCalls.length} 个真实 fast_context 调用（去重后），本次 replay ${calls.length} 个`);

const runA = ARM_FILTER === "both" || ARM_FILTER === "A";
const runB = ARM_FILTER === "both" || ARM_FILTER === "B";

const resultA = runA ? await replayArm(calls, "A") : null;
const resultB = runB ? await replayArm(calls, "B") : null;

const summaryA = resultA ? summarize(resultA) : null;
const summaryB = resultB ? summarize(resultB) : null;

const report = {
  ranAt: new Date().toISOString(),
  armA: "all features OFF (NOVA_CTX_*=0)",
  armB: "all features ON (default)",
  callCount: calls.length,
  summaryA,
  summaryB,
  cases: calls.map((call, i) => ({
    keywords: call.params.keywords,
    session: call.session,
    A: resultA?.[i] ? { wallMs: resultA[i].wallMs, files: resultA[i].files, blocks: resultA[i].blocks, bytes: resultA[i].bytes, miss: resultA[i].miss, error: resultA[i].error } : null,
    B: resultB?.[i] ? { wallMs: resultB[i].wallMs, files: resultB[i].files, blocks: resultB[i].blocks, bytes: resultB[i].bytes, miss: resultB[i].miss, error: resultB[i].error } : null,
  })),
};
writeFileSync(REPORT_JSON, JSON.stringify(report, null, 2));

// ---------- Markdown 报告 ----------
const lines = [
  "# 真实会话 Replay A/B 报告",
  "",
  `- 数据来源：\`~/.nova/alkaid/sessions\` 最近 50 个会话中提取的 ${calls.length} 个去重 fast_context 调用`,
  `- A 臂（对照）：全部新特性关闭（\`NOVA_CTX_SUBMODULAR=0 MERGED_SEARCH=0 DEFS_SUGGEST=0 PREFETCH=0 SPECULATIVE=0\`）`,
  `- B 臂（实验）：全部新特性开启（默认）`,
  "",
  "## 汇总",
  "",
  "| 指标 | A（旧） | B（新） | Δ | Δ% |",
  "|---|---:|---:|---:|---:|",
];
if (summaryA && summaryB) {
  const metrics = [
    ["成功率", `${summaryA.ok}/${summaryA.total}`, `${summaryB.ok}/${summaryB.total}`, null, null],
    ["MISS 数", summaryA.miss, summaryB.miss, summaryB.miss - summaryA.miss, summaryA.miss ? `${((summaryB.miss - summaryA.miss) / summaryA.miss * 100).toFixed(1)}%` : "n/a"],
    ["平均耗时(ms)", summaryA.avgWallMs, summaryB.avgWallMs, summaryB.avgWallMs - summaryA.avgWallMs, summaryA.avgWallMs ? `${((summaryB.avgWallMs - summaryA.avgWallMs) / summaryA.avgWallMs * 100).toFixed(1)}%` : "n/a"],
    ["总耗时(ms)", summaryA.totalWallMs, summaryB.totalWallMs, summaryB.totalWallMs - summaryA.totalWallMs, summaryA.totalWallMs ? `${((summaryB.totalWallMs - summaryA.totalWallMs) / summaryA.totalWallMs * 100).toFixed(1)}%` : "n/a"],
    ["平均文件数", summaryA.avgFiles, summaryB.avgFiles, +(summaryB.avgFiles - summaryA.avgFiles).toFixed(1), summaryA.avgFiles ? `${((summaryB.avgFiles - summaryA.avgFiles) / summaryA.avgFiles * 100).toFixed(1)}%` : "n/a"],
    ["平均块数", summaryA.avgBlocks, summaryB.avgBlocks, +(summaryB.avgBlocks - summaryA.avgBlocks).toFixed(1), summaryA.avgBlocks ? `${((summaryB.avgBlocks - summaryA.avgBlocks) / summaryA.avgBlocks * 100).toFixed(1)}%` : "n/a"],
    ["平均字节数", summaryA.avgBytes, summaryB.avgBytes, summaryB.avgBytes - summaryA.avgBytes, summaryA.avgBytes ? `${((summaryB.avgBytes - summaryA.avgBytes) / summaryA.avgBytes * 100).toFixed(1)}%` : "n/a"],
    ["平均 SPECULATIVE 块", summaryA.avgSpeculative, summaryB.avgSpeculative, +(summaryB.avgSpeculative - summaryA.avgSpeculative).toFixed(1), "n/a"],
    ["平均 SIG 签名", summaryA.avgSig, summaryB.avgSig, +(summaryB.avgSig - summaryA.avgSig).toFixed(1), "n/a"],
    ["平均 IMPACT 行", summaryA.avgImpact, summaryB.avgImpact, +(summaryB.avgImpact - summaryA.avgImpact).toFixed(1), "n/a"],
  ];
  for (const [label, a, b, delta, pct] of metrics) {
    const d = delta === null ? "" : (typeof delta === "number" && delta > 0 ? `+${delta}` : `${delta}`);
    lines.push(`| ${label} | ${a} | ${b} | ${d} | ${pct ?? ""} |`);
  }
}
lines.push("", "## 分案例", "", "| # | 关键词 | A 文件/块/字节 | B 文件/块/字节 | A 耗时 | B 耗时 | Δ 耗时 |", "|---|---|---:|---:|---:|---:|---:|");
for (let i = 0; i < calls.length; i++) {
  const a = resultA?.[i];
  const b = resultB?.[i];
  const kw = calls[i].params.keywords.join(", ").slice(0, 40);
  lines.push(`| ${i + 1} | ${kw} | ${a ? `${a.files}/${a.blocks}/${a.bytes}` : "-"} | ${b ? `${b.files}/${b.blocks}/${b.bytes}` : "-"} | ${a?.wallMs ?? "-"}ms | ${b?.wallMs ?? "-"}ms | ${a && b ? `${b.wallMs - a.wallMs > 0 ? "+" : ""}${b.wallMs - a.wallMs}ms` : "-"} |`);
}
lines.push("", "## 判读口径", "",
  "- **平均耗时**：B 臂应更低（Lazy Greedy + 合并扫描 + 预取生效）。",
  "- **平均文件数/块数**：B 臂应更高或持平（次模覆盖多样性更好）。",
  "- **SPECULATIVE 块**：B 臂独有（A 臂关闭），数量反映超取经济学生效程度。",
  "- **MISS 数**：两臂应一致（打包算法不影响 MISS 判定）。",
  "- **错误数**：两臂都应为 0。",
);
writeFileSync(REPORT_MD, lines.join("\n"));

console.log(JSON.stringify({ report: REPORT_JSON, markdown: REPORT_MD, summaryA, summaryB }, null, 2));
