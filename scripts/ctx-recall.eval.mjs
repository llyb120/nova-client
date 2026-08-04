// ctx-recall.eval.mjs — fast_context 召回率评测（真实仓库，非夹具）
//
// 用法: node scripts/ctx-recall.eval.mjs [--json]
//
// 每个用例声明一组期望出现在输出里的目标（文件路径 / 符号名），
// recall = 命中目标数 / 期望目标数。MISS 用例断言返回紧凑 MISS 且带 did-you-mean。
// 任何用例低于自己的 min 阈值时进程以 1 退出，可接入 CI 监控检索回归。

import { repoRoot } from "./ctx-core.mjs";

const { contextBundle } = await import("./ctx-core.mjs");

const CASES = [
  {
    // 历史失败案例：agent 臆造 PlanMode/agentMode，旧版整体 MISS。
    // 软降级后应由短语泛词 plan/build 召回模式实现。modeChoices 是拉伸目标：
    // 高频泛词 build/mode 会把 build.rs 等同名文件顶进 EDIT 槽位，属已知排序噪声。
    name: "plan-mode 原始失败查询（软降级）",
    args: {
      keywords: ["plan mode", "build mode", "/plan", "PlanMode", "agentMode"],
      task: "Remove plan mode UI selection; keep only build as default; plan only via /plan slash command",
      budget: 800,
    },
    expect: ["src/store.ts", "PlanActionCard.tsx", "implementProposedPlan", "modeChoices"],
    min: 0.75,
  },
  {
    name: "真实符号直查（对照组）",
    args: { keywords: ["modeChoices", "UNIFIED_MODES"] },
    expect: ["src/store.ts", "modeChoices"],
    min: 1,
  },
  {
    name: "臆造锚点 + 真实短语泛词混合",
    args: {
      keywords: ["ThreadTitleGenerator", "derive title"],
      task: "会话标题生成逻辑调整",
    },
    expect: ["derive_title", "src-tauri/src/opencode_sdk.rs"],
    min: 0.5,
  },
  {
    name: "核心符号定位",
    args: { keywords: ["contextBundle"] },
    expect: ["scripts/ctx-core.mjs", "contextBundle"],
    min: 1,
  },
  {
    name: "UI 组件符号定位",
    args: { keywords: ["slashSuggestions"] },
    expect: ["src/components/slashSuggestions.ts"],
    min: 1,
  },
  {
    name: "CJK task 召回（无关键词）",
    args: { task: "界面可选会话模式只保留 Build" },
    expect: ["src/store.ts", "UNIFIED_MODES"],
    min: 0.5,
  },
  {
    // 真正不存在的符号：仍应硬 MISS，且 did-you-mean 给出真实相近符号。
    name: "真 MISS + did-you-mean",
    args: { keywords: ["agentMode"], task: "switch agent mode handling" },
    miss: true,
    expectMiss: [
      /^# CTX MISS/,
      /did-you-mean: [^\n]*\([^\n)]*:\d+\)/, // 建议带 path:ln，可直接复査
      /did-you-mean: [^\n]*agent/i, // 建议与锚点词相关
    ],
  },
];

async function main() {
  const root = repoRoot();
  const json = process.argv.includes("--json");
  const rows = [];
  let failures = 0;

  for (const item of CASES) {
    const started = performance.now();
    const out = await contextBundle(item.args, root);
    const ms = Math.round(performance.now() - started);

    if (item.miss) {
      const missing = item.expectMiss.filter((re) => !re.test(out));
      const ok = missing.length === 0;
      if (!ok) failures += 1;
      rows.push({ name: item.name, kind: "miss", ok, ms, missing: missing.map(String), recall: ok ? 1 : 0 });
      continue;
    }

    const hit = item.expect.filter((target) => out.includes(target));
    const missing = item.expect.filter((target) => !out.includes(target));
    const recall = hit.length / item.expect.length;
    const ok = recall >= item.min - 1e-9;
    if (!ok) failures += 1;
    rows.push({ name: item.name, kind: "hit", ok, ms, recall, min: item.min, hit, missing });
  }

  const recallCases = rows.filter((row) => row.kind === "hit");
  const meanRecall = recallCases.reduce((sum, row) => sum + row.recall, 0) / Math.max(1, recallCases.length);

  if (json) {
    console.log(JSON.stringify({ root, meanRecall, failures, rows }, null, 2));
  } else {
    console.log(`root: ${root}`);
    for (const row of rows) {
      if (row.kind === "miss") {
        console.log(`${row.ok ? "PASS" : "FAIL"}  [miss] ${row.name}  (${row.ms}ms)${row.ok ? "" : `  缺少: ${row.missing.join(", ")}`}`);
      } else {
        const pct = `${Math.round(row.recall * 100)}%`.padStart(4);
        console.log(`${row.ok ? "PASS" : "FAIL"}  ${pct}  ${row.name}  (${row.ms}ms)${row.missing.length ? `  未召回: ${row.missing.join(", ")}` : ""}`);
      }
    }
    console.log(`\nmean recall(hit 用例): ${(meanRecall * 100).toFixed(1)}%  failures: ${failures}`);
  }
  process.exit(failures ? 1 : 0);
}

await main();
