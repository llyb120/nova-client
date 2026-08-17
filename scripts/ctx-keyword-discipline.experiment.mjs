// ctx-keyword-discipline.experiment.mjs — 验证"关键词纪律"能提升多少真实召回
//
// 用录制的真实 polaris 调用（真实 rawInput + 真实编辑目标）在当前 native
// 实现上做三组对照回放：
//   A 原始查询        ：完全按录制参数回放（baseline）
//   B 纪律查询(机械版)：删掉仓库中零命中的臆造关键词，保留真实关键词
//   C 纪律查询(上界)  ：关键词替换为目标文件里真实存在的导出符号（oracle）
// 三组同一调用同一 cwd，相对差值即"查询质量"对召回的净贡献。

import { readdirSync, readFileSync, existsSync } from "node:fs";
import { join } from "node:path";
import { homedir } from "node:os";
import { execFileSync } from "node:child_process";
import { analyzeThreadForDbg } from "./ctx-invoke.eval.mjs";
import { callGlobalContextTool } from "./nova-context-client.mjs";

const dir = process.argv[2]?.replace(/^~(?=\/|$)/, homedir()) || join(homedir(), ".nova", "threads");

function filesInOutput(out) {
  const set = new Set();
  for (const m of String(out).matchAll(/^###\s+(\S+)\s+\(/gm)) set.add(m[1]);
  return set;
}

// 关键词在仓库是否有字面命中（遵守 .gitignore，与引擎 rg 同口径）
function keywordExists(root, kw) {
  try {
    execFileSync("rg", ["-c", "--no-messages", "--fixed-strings", "--", kw, root], { stdio: ["ignore", "pipe", "ignore"], timeout: 8000 });
    return true;
  } catch { return false; }
}

// oracle：目标文件里真实存在的导出符号（优先 export function/const/interface/type/class）
function oracleSymbols(root, targets) {
  const syms = [];
  for (const t of targets.slice(0, 6)) {
    let text; try { text = readFileSync(join(root, t), "utf8"); } catch { continue; }
    for (const m of text.matchAll(/^export\s+(?:async\s+)?(?:function|const|class|interface|type)\s+([A-Za-z_$][\w$]*)/gm)) {
      if (!["default"].includes(m[1])) syms.push(m[1]);
    }
    if (syms.length >= 6) break;
  }
  return [...new Set(syms)].slice(0, 5);
}

const calls = [];
for (const f of readdirSync(dir).filter((x) => x.endsWith(".json"))) {
  let d; try { d = JSON.parse(readFileSync(join(dir, f), "utf8")); } catch { continue; }
  for (const t of analyzeThreadForDbg(d).turns) {
    for (const c of t.fcCalls) {
      if (!c.targets.length || !t.cwd || !existsSync(t.cwd)) continue;
      if (!c.rawInput || (!c.rawInput.keywords && !c.rawInput.task)) continue;
      // 只回放真正的 polaris 查询（带 keywords/task），跳过被误录的 shell
      if (!Array.isArray(c.rawInput.keywords) && !c.rawInput.task) continue;
      calls.push({ t, c });
    }
  }
}
calls.sort((a, b) => b.c.targets.length - a.c.targets.length);
const budget = Number(process.argv[3] ?? 24);
const picked = calls.slice(0, budget);
console.log(`候选 ${calls.length} 次调用，回放前 ${picked.length} 次（按目标文件数排序）\n`);

const rows = [];
for (const { t, c } of picked) {
  const root = t.cwd;
  const recall = (out) => {
    const fs = filesInOutput(out);
    return c.targets.filter((f) => fs.has(f)).length / c.targets.length;
  };
  const baseArgs = { ...c.rawInput };
  delete baseArgs.command;
  const rA = recall(await callGlobalContextTool("polaris", root, baseArgs));

  const kws = Array.isArray(baseArgs.keywords) ? baseArgs.keywords : [];
  const kept = kws.filter((k) => keywordExists(root, k));
  const dropped = kws.length - kept.length;
  const argsB = { ...baseArgs, keywords: kept.length ? kept : kws.slice(0, 1) };
  const rB = recall(await callGlobalContextTool("polaris", root, argsB));

  const oracle = oracleSymbols(root, c.targets);
  const argsC = { ...baseArgs, keywords: oracle.length ? oracle : kept, files: undefined };
  const rC = oracle.length ? recall(await callGlobalContextTool("polaris", root, argsC)) : rB;

  rows.push({ task: t.task, root: root.split("/").pop(), n: c.targets.length, dropped, total: kws.length, rA, rB, rC });
  console.log(`${(rA * 100).toFixed(0).padStart(3)}% → ${(rB * 100).toFixed(0).padStart(3)}% → ${(rC * 100).toFixed(0).padStart(3)}%  删${dropped}/${kws.length}个  ${t.task.slice(0, 44)}`);
}

const mean = (k, rs = rows) => (rs.length ? rs.reduce((s, r) => s + r[k], 0) / rs.length : NaN);
const fmt = (x) => (Number.isNaN(x) ? "n/a" : `${(x * 100).toFixed(1)}%`);
const failing = rows.filter((r) => r.rA < 1);
console.log(`\n全部 ${rows.length} 次调用:`);
console.log(`A 原始查询召回:        ${fmt(mean("rA"))}`);
console.log(`B 删臆造关键词召回:    ${fmt(mean("rB"))}  (Δ ${((mean("rB") - mean("rA")) * 100).toFixed(1)}pt)`);
console.log(`C 真实符号上界召回:    ${fmt(mean("rC"))}  (Δ ${((mean("rC") - mean("rA")) * 100).toFixed(1)}pt)`);
if (failing.length) {
  console.log(`\n仅原始未召满的 ${failing.length} 次调用:`);
  console.log(`A: ${fmt(mean("rA", failing))}  B: ${fmt(mean("rB", failing))} (Δ ${((mean("rB", failing) - mean("rA", failing)) * 100).toFixed(1)}pt)  C: ${fmt(mean("rC", failing))} (Δ ${((mean("rC", failing) - mean("rA", failing)) * 100).toFixed(1)}pt)`);
}
console.log(`平均每次调用臆造关键词: ${(rows.reduce((s, r) => s + r.dropped, 0) / rows.length).toFixed(1)} / ${(rows.reduce((s, r) => s + r.total, 0) / rows.length).toFixed(1)} 个`);
