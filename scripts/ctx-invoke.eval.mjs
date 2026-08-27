// ctx-invoke.eval.mjs — polaris 调用率 + 真实任务召回评测（真实会话回放）
//
// 用法: node scripts/ctx-invoke.eval.mjs [--json] [--dir ~/.nova/threads] [--limit N] [--replay [N]]
//
// 数据来源：真实会话线程文件（items 里的 tool 记录带 rawInput/rawOutput）。
//
// 指标定义：
// - coding turn   : 有编辑(edit/write)或检索(read/rg/grep/find)动作的用户回合
// - shouldCall    : 该回合编辑了 >=2 个不同文件（跨文件任务 = polaris 的目标场景）
// - invoke rate   : 调用了 polaris 的回合 / 全部 coding turn
// - sc invoke rate: shouldCall 回合里实际调用的比例（核心指标：该调时调了没有）
// - waste         : shouldCall 但没调的回合，平均白跑的检索动作数（rg/grep/find/read）
// - recall        : 调用点之后、同回合内新编辑的文件（调用前已编辑的视为已知路径，剔除），
//                   有多少出现在 polaris 输出的 ### 文件块里。
// --replay [N]    : 用录制的真实 rawInput 对当前 native 实现重跑（cwd 需仍存在），
//                   对比"录制时召回"与"当前实现召回"，检测检索回归/改进。

import { readdirSync, readFileSync, existsSync, statSync, realpathSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { join, relative, isAbsolute } from "node:path";
import { homedir } from "node:os";
import { callGlobalContextTool } from "./nova-context-client.mjs";

const SEARCH_CMD = /^\s*(rg|grep|git\s+grep|find|ag|ls|fd)\b/;

function classify(item) {
  const title = item.title || "";
  // 工具名只在 "·" 前缀段匹配；shell 命令正文里提到 polaris 不算调用
  const head = title.split("·")[0];
  const cmd = title.includes("·") ? title.split("·").slice(1).join("·").trim() : "";
  if (/\bpolaris\b/.test(head)) return { tool: "polaris" };
  if (/\b(edit|write)\b/.test(head) || /^修改 /.test(title)) {
    // 单文件 edit：rawInput.path；批量：rawInput.files[].path
    const filesArr = Array.isArray(item.rawInput?.files) ? item.rawInput.files : null;
    const paths = filesArr?.map((f) => f.path).filter(Boolean);
    if (paths?.length) return { tool: "edit", paths };
    const p = item.rawInput?.path || cmd;
    return { tool: "edit", paths: p ? [p] : [] };
  }
  if (/\bread_files\b/.test(head)) {
    const paths = (item.rawInput?.paths || []).map((x) => x.path).filter(Boolean);
    return { tool: "read", n: Math.max(1, paths.length) };
  }
  if (/\bread\b/.test(head)) return { tool: "read", n: 1 };
  if (/\b(grep|glob|search)\b/.test(head) || item.kind === "search") return { tool: "search_cmd" };
  if (/\b(shell|bash)\b/.test(head) || item.kind === "execute") {
    if (SEARCH_CMD.test(cmd) || SEARCH_CMD.test(title)) return { tool: "search_cmd" };
    return { tool: "bash_other" };
  }
  return { tool: "other" };
}

// 深度收集 rawOutput 里的所有字符串并拼回真实文本（JSON 转义的 \n 还原）
function collectStrings(o, out = []) {
  if (typeof o === "string") out.push(o);
  else if (Array.isArray(o)) o.forEach((x) => collectStrings(x, out));
  else if (o && typeof o === "object") Object.values(o).forEach((x) => collectStrings(x, out));
  return out;
}

function rel(p, cwd) {
  if (!p) return null;
  if (!isAbsolute(p)) return p;
  const r = relative(cwd, p);
  return r.startsWith("..") ? p : r;
}

// 解析 polaris 输出里的 "### <path> (...)" 文件块标题
function filesInOutput(out) {
  const set = new Set();
  for (const m of String(out).matchAll(/^###\s+(\S+)\s+\(/gm)) set.add(m[1]);
  return set;
}

const key = (it) => it.id ?? it.ts ?? 0;

function analyzeThread(d) {
  const cwd = d.cwd || "";
  const items = [...(d.items || [])].sort((a, b) => key(a) - key(b));
  const turns = [];
  let cur = null;
  for (const it of items) {
    if (it.type === "user") {
      cur = { task: (it.text || "").slice(0, 80), events: [] };
      turns.push(cur);
    } else if (it.type === "tool" && cur) {
      cur.events.push(it);
    }
  }
  const turnStats = [];
  for (const turn of turns) {
    const edited = new Set();
    let search = 0, fc = 0;
    const fcCalls = [];
    for (const ev of turn.events) {
      const c = classify(ev);
      if (c.tool === "edit") (c.paths || []).forEach((p) => edited.add(rel(p, cwd) || p));
      else if (c.tool === "read") search += c.n || 1;
      else if (c.tool === "search_cmd") search += 1;
      else if (c.tool === "polaris") { fc += 1; fcCalls.push(ev); }
    }
    if (edited.size === 0 && search === 0) continue; // 非 coding 回合

    // 每次 polaris 调用的召回口径：调用之后新编辑的文件（调用前已编辑 = 已知路径）
    for (const call of fcCalls) {
      const before = new Set(), after = new Set();
      for (const ev of turn.events) {
        const c = classify(ev);
        if (c.tool !== "edit") continue;
        for (const p of c.paths || []) {
          const r = rel(p, cwd) || p;
          (key(ev) < key(call) ? before : after).add(r);
        }
      }
      call.targets = [...after].filter((f) => !before.has(f));
      // 遥测兜底：旧会话只有 content 没有 rawOutput；两者都取
      const outText = [collectStrings(call.rawOutput).join("\n"), collectStrings(call.content).join("\n")].join("\n");
      call.outLen = outText.length;
      call.outFiles = filesInOutput(outText);
      call.recalled = call.targets.filter((f) => call.outFiles.has(f));
    }
    turnStats.push({
      agent: d.agentKind || "?", cwd, task: turn.task,
      edited: edited.size, search, fc,
      shouldCall: edited.size >= 2,
      invoked: fc > 0,
      fcCalls,
    });
  }
  return { cwd, agent: d.agentKind, turns: turnStats };
}

const mean = (xs) => (xs.length ? xs.reduce((s, x) => s + x, 0) / xs.length : NaN);
const pct = (x) => (Number.isNaN(x) ? "n/a" : `${(x * 100).toFixed(1)}%`);

async function main() {
  const argv = process.argv.slice(2);
  const json = argv.includes("--json");
  const dirIdx = argv.indexOf("--dir");
  const limIdx = argv.indexOf("--limit");
  const replayIdx = argv.indexOf("--replay");
  const dir = dirIdx >= 0 ? argv[dirIdx + 1].replace(/^~(?=\/|$)/, homedir()) : join(homedir(), ".nova", "threads");
  const limit = limIdx >= 0 ? Number(argv[limIdx + 1]) : Infinity;
  const replay = replayIdx >= 0 ? Number(argv[replayIdx + 1] ?? Infinity) || Infinity : 0;

  const files = readdirSync(dir).filter((f) => f.endsWith(".json"))
    .sort((a, b) => statSync(join(dir, b)).mtimeMs - statSync(join(dir, a)).mtimeMs)
    .slice(0, limit);

  const all = [];
  for (const f of files) {
    try { all.push(analyzeThread(JSON.parse(readFileSync(join(dir, f), "utf8")))); } catch { /* 跳过损坏线程 */ }
  }
  const turns = all.flatMap((t) => t.turns);
  const sc = turns.filter((t) => t.shouldCall);
  const scInvoked = sc.filter((t) => t.invoked);
  const wasted = sc.filter((t) => !t.invoked);
  const calls = turns.flatMap((t) => t.fcCalls.map((c) => ({ t, c })));
  const evaluable = calls.filter(({ c }) => c.targets.length > 0 && c.outLen > 0);
  const emptyOut = calls.filter(({ c }) => c.outLen === 0).length;
  const recallMean = mean(evaluable.map(({ c }) => c.recalled.length / c.targets.length));

  const byAgent = {};
  for (const t of turns) {
    const a = byAgent[t.agent] ??= { coding: 0, invoked: 0, sc: 0, scInvoked: 0 };
    a.coding += 1; if (t.invoked) a.invoked += 1;
    if (t.shouldCall) { a.sc += 1; if (t.invoked) a.scInvoked += 1; }
  }

  // --replay：用录制的真实 rawInput 对当前 native 实现重跑（含录制时输出为空的调用）
  let replayStats = null;
  if (replay > 0) {
    const rows = [];
    let budget = replay;
    for (const { t, c } of calls.filter(({ c }) => c.targets.length > 0)) {
      if (budget <= 0) break;
      if (!c.rawInput || !t.cwd || !existsSync(t.cwd)) continue;
      budget -= 1;
      try {
        const out = await callGlobalContextTool("polaris", t.cwd, c.rawInput);
        const outFiles = filesInOutput(out);
        const hit = c.targets.filter((f) => outFiles.has(f)).length;
        rows.push({ task: t.task, cwd: t.cwd, targets: c.targets.length, recorded: c.outLen > 0 ? c.recalled.length : null, current: hit });
      } catch (e) { rows.push({ task: t.task, cwd: t.cwd, error: String(e).slice(0, 120) }); }
    }
    const ok = rows.filter((r) => r.error == null);
    const recOk = ok.filter((r) => r.recorded != null);
    replayStats = {
      tried: rows.length, errors: rows.filter((r) => r.error).length,
      recordedMean: mean(recOk.map((r) => r.recorded / r.targets)),
      currentMean: mean(ok.map((r) => r.current / r.targets)),
      rows,
    };
  }

  const result = {
    threads: all.length, codingTurns: turns.length, fcCallsTotal: turns.reduce((s, t) => s + t.fc, 0),
    invokeRate: turns.length ? turns.filter((t) => t.invoked).length / turns.length : NaN,
    shouldCallTurns: sc.length,
    scInvokeRate: sc.length ? scInvoked.length / sc.length : NaN,
    avgWastedSearch: mean(wasted.map((t) => t.search)),
    recallEvaluableCalls: evaluable.length, recallMean, emptyOutputCalls: emptyOut,
    byAgent, replay: replayStats,
  };

  if (json) { console.log(JSON.stringify(result, null, 2)); return; }
  console.log(`线程: ${all.length}  coding 回合: ${turns.length}  polaris 调用: ${result.fcCallsTotal}`);
  console.log(`调用率(全部 coding 回合): ${pct(result.invokeRate)}`);
  console.log(`调用率(shouldCall 回合, 核心): ${pct(result.scInvokeRate)}  (${scInvoked.length}/${sc.length})`);
  console.log(`shouldCall 未调用回合白跑检索均值: ${Number.isNaN(result.avgWastedSearch) ? "n/a" : result.avgWastedSearch.toFixed(1)} 次/回合`);
  console.log(`召回率(调用后有新编辑文件且有输出记录的 ${evaluable.length} 次调用): ${pct(recallMean)}  (无输出记录: ${emptyOut} 次, 不计入)`);
  console.log("按 agent:");
  for (const [k, a] of Object.entries(byAgent)) {
    console.log(`  ${k.padEnd(10)} coding=${a.coding} invoked=${a.invoked} shouldCall=${a.sc} scInvoked=${a.scInvoked}`);
  }
  if (replayStats) {
    console.log(`replay(n=${replayStats.tried - replayStats.errors}, errors=${replayStats.errors}): 录制时召回=${pct(replayStats.recordedMean)}  当前实现召回=${pct(replayStats.currentMean)}`);
    for (const r of replayStats.rows.filter((x) => x.error == null && (x.recorded == null || x.current !== x.recorded))) {
      console.log(`  ${r.recorded == null ? "?" : r.recorded}/${r.targets}→${r.current}/${r.targets}  ${r.task}`);
    }
  }
}

export { analyzeThread as analyzeThreadForDbg };

const isMain = process.argv[1] && realpathSync(process.argv[1]) === realpathSync(fileURLToPath(import.meta.url));
if (isMain) await main();
