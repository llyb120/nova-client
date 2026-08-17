// ctx-recall.eval.mjs — polaris 召回率评测（native Rust 实现，真实仓库 + 夹具）
//
// 用法: node scripts/ctx-recall.eval.mjs [--json] [--save <file>]
//
// 每个用例声明一组期望出现在输出里的目标（文件路径 / 符号名 / 标记），
// recall = 命中目标数 / 期望目标数。MISS 用例断言返回紧凑 MISS 且带 did-you-mean。
// 任何用例低于自己的 min 阈值时进程以 1 退出，可接入 CI 监控检索回归。
//
// 评测对象为线上 Rust 实现（nova-tools-napi）。JS 镜像已移除。
// 夹具用例在临时目录构造确定性迷你仓库，专测反向 import 图、签名依赖、预算回填。

import { mkdtempSync, mkdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { repoRoot } from "./ctx-core.mjs";
import { callGlobalContextTool } from "./nova-context-client.mjs";

function makeFixture(files) {
  const dir = mkdtempSync(join(tmpdir(), "ctx-recall-"));
  for (const [name, content] of Object.entries(files)) {
    const path = join(dir, ...name.split("/"));
    mkdirSync(join(path, ".."), { recursive: true });
    writeFileSync(path, content);
  }
  return dir;
}

// 反向 import 图夹具：别名导入 + barrel 改名透传，纯文本检索对 mountB/widget( 完全不可见。
const GRAPH_FIXTURE = () => makeFixture({
  "src/core.ts": [
    "export interface WidgetConfig {",
    "  title: string;",
    "  mode: string;",
    "}",
    "export function renderWidget(config: WidgetConfig) {",
    "  return `<div>${config.title}:${config.mode}</div>`;",
    "}",
    "",
  ].join("\n"),
  "src/alias.ts": [
    'import { renderWidget as showWidget } from "./core";',
    "",
    "export function mountAliasPanel(config) {",
    "  const html = showWidget(config);",
    "  return html.toUpperCase();",
    "}",
    "",
  ].join("\n"),
  "src/barrel.ts": 'export { renderWidget as widget } from "./core";\n',
  "src/deep.ts": [
    'import { widget } from "./barrel";',
    "",
    "export function mountB(config) {",
    "  return widget(config);",
    "}",
    "",
  ].join("\n"),
});

// 签名类型依赖夹具：WidgetConfig 只出现在签名里，与 10 个正文 helper 竞争 8 个 DEPS 槽位。
// helper 直接从各自文件导入（可 resolve），正文调用得分 17 > 签名类型 9，基线下 WidgetConfig 被挤出。
const SIG_DEPS_FIXTURE = () => {
  const imports = [];
  const calls = [];
  const files = {
    "src/types.ts": "export interface WidgetConfig {\n  title: string;\n  mode: string;\n  retries: number;\n}\n",
  };
  for (let i = 0; i < 10; i += 1) {
    files[`src/h${i}.ts`] = `export function helperAlpha${i}(value) {\n  return value + ${i};\n}\n`;
    imports.push(`import { helperAlpha${i} } from "./h${i}";`);
    calls.push(`  acc = helperAlpha${i}(acc);`);
  }
  files["src/service.ts"] = [
    'import { WidgetConfig } from "./types";',
    ...imports,
    "",
    "export function mountWidget(config: WidgetConfig) {",
    "  let acc = config.retries;",
    ...calls,
    "  return { title: config.title, mode: config.mode, acc };",
    "}",
    "",
  ].join("\n");
  return makeFixture(files);
};

// 预算回填夹具：target 定义 + 3 文件 × 4 调用函数，总量远超 8KB 硬顶。
// 打包按 soft(64%) 封顶暂缓一批块；回填应在硬顶内把暂缓块补回。
const BACKFILL_FIXTURE = () => {
  const files = {
    "src/core.ts": "export function alphaTarget(v) {\n  return v * 2;\n}\n",
  };
  for (let m = 0; m < 3; m += 1) {
    const units = [`import { alphaTarget } from "./core";`, ""];
    for (let n = 0; n < 4; n += 1) {
      const pad = [];
      for (let i = 0; i < 22; i += 1) pad.push(`  const pad${m}_${n}_${i} = "padding-value-${m}-${n}-${i}-aaaaaaaaaaaaaaaaaaaaaaaa";`);
      units.push(
        `export function useTarget${m}_${n}() {`,
        `  // block-${m}-${n}-marker`,
        ...pad,
        `  return alphaTarget(${n});`,
        "}",
        "",
      );
    }
    files[`src/mod${m}.ts`] = units.join("\n");
  }
  return makeFixture(files);
};

const CASES = [
  {
    // 历史失败案例：agent 臆造 PlanMode/agentMode，旧版整体 MISS。
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
    args: { keywords: ["collect_dependencies"] },
    expect: ["src-tauri/src/nova_tools_native/context.rs", "collect_dependencies"],
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
    // 注意：若 did-you-mean 里有编辑距离足够小的符号，会自动锚点更正重试而不再 MISS，
    // 因此该用例要求 repo 内不存在与 agentMode 相近的符号。
    name: "真 MISS + did-you-mean",
    args: { keywords: ["agentMode"], task: "switch agent mode handling" },
    miss: true,
    expectMiss: [
      /^# CTX MISS/,
      /did-you-mean: [^\n]*\([^\n)]*:\d+\)/, // 建议带 path:ln，可直接复査
      /did-you-mean: [^\n]*agent/i, // 建议与锚点词相关
    ],
  },
  {
    // 特性 2：锚点 typo 自动更正重试。modeChoics → modeChoices（编辑距离 1）。
    // 期望值用拼接避免本文件被 fast_context 当成命中源（自引用污染）。
    name: "锚点 typo 自动更正",
    args: { keywords: ["modeChoics"], task: "switch mode handling" },
    expect: ["锚点更" + "正", "modeChoices", "src/store.ts"],
    min: 1,
  },
  {
    // 特性 1：反向 import 图。别名调用点 showWidget( 与 barrel 改名透传 mountB/widget(
    // 对纯文本检索不可见；import 图应精确召回并打 [import图] 标记。
    name: "反向 import 图（别名 + barrel 透传）",
    fixture: GRAPH_FIXTURE,
    args: { keywords: ["renderWidget"], task: "找出所有调用方 caller 兼容" },
    expect: ["core.ts", "showWidget(", "mountB", "widget(", "[import" + "图]"],
    min: 1,
  },
  {
    // 特性 3：种子签名里的类型名提权。WidgetConfig 仅在签名出现，
    // 与 10 个正文 helper 竞争时应进入 DEPS。
    name: "签名类型依赖提权",
    fixture: SIG_DEPS_FIXTURE,
    args: { keywords: ["mountWidget"], task: "修改挂载入口签名" },
    expect: ["### src/types.ts", "WidgetConfig"],
    min: 1,
  },
  {
    // 特性 5：预算回填。shrink 删除 FULL 大文件后，用回填把高价值块补回预算。
    name: "预算回填（shrink 超调回补）",
    fixture: BACKFILL_FIXTURE,
    args: { keywords: ["alphaTarget"], maxBytes: 8192 },
    expect: ["block-0-0-marker", "block-0-1-marker", "block-0-2-marker", "block-1-0-marker"],
    min: 1,
  },
  {
    // 特性 4：git 共改耦合（可选开关）。真实仓库 git 历史应给出共改文件提示。
    name: "git 共改耦合提示",
    args: { keywords: ["modeChoices"], coupling: true, task: "找出所有调用方 caller" },
    expect: ["共改耦合(" + "git)"],
    min: 1,
  },
  // ---- 真实会话回放回归用例（来自 ctx-invoke.eval.mjs 的低召回任务，符号已适配当前仓库） ----
  {
    // 真实任务“为工作流节点增加人工审核”，原录制 11 目标文件只召回 0。
    // 期望跨 runtime/types/UI 三个层面同时召回。
    name: "真实回放：工作流人工审核（跨层多文件）",
    args: {
      keywords: ["manualWorkflowReview", "WorkflowSettingsPanel", "handleTurnEnd", "workflow"],
      task: "为工作流节点增加人工审核：节点开启后 turn 由人确认",
      budget: 800,
    },
    expect: ["src/workflow/runtime.ts", "src/workflow/types.ts", "src/components/WorkflowSettingsPanel.tsx"],
    min: 1,
  },
  {
    // 真实任务“简化工作流配置”，CJK+英文泛词混合，无强锚点符号。
    name: "真实回放：工作流配置简化（泛词混合）",
    args: {
      keywords: ["workflow", "工作流", "transition", "handoff"],
      task: "简化工作流配置：节点和连线提示词 + 引擎隐式接力会话结论",
      budget: 800,
    },
    expect: ["src/workflow/types.ts", "src/workflow/runtime.ts", "src/components/WorkflowCanvas.tsx", "src/components/WorkflowSettingsPanel.tsx"],
    min: 0.75,
  },
  {
    // 真实任务“那就加一句保底”，原录制 3 目标只召回 1：测试文件不进闭包。
    // 期望实现文件与其测试文件一并召回。
    name: "真实回放：提示词保底（测试文件召回）",
    args: {
      files: ["scripts/alkaid-core.mjs", "scripts/cursor-filesystem-tools.mjs", "docs/alkaid.md"],
      keywords: ["buildAlkaidSystemPrompt", "cursorPromptPrefix", "同轮并行"],
      task: "在并行工具规则里加一句保底说明",
      budget: 800,
    },
    expect: ["scripts/alkaid-core.mjs", "scripts/alkaid" + ".test.mjs", "scripts/alkaid-context-super" + ".test.mjs"],
    min: 1,
  },
];

async function main() {
  const root = repoRoot();
  const json = process.argv.includes("--json");
  const saveIndex = process.argv.indexOf("--save");
  const rows = [];
  let failures = 0;

  for (const item of CASES) {
    const caseRoot = item.fixture ? item.fixture() : root;
    const started = performance.now();
    const out = await callGlobalContextTool("polaris", caseRoot, item.args);
    const ms = Math.round(performance.now() - started);
    const kb = Math.round(out.length / 102.4) / 10;

    if (item.miss) {
      const missing = item.expectMiss.filter((re) => !re.test(out));
      const ok = missing.length === 0;
      if (!ok) failures += 1;
      rows.push({ name: item.name, kind: "miss", ok, ms, kb, missing: missing.map(String), recall: ok ? 1 : 0 });
      continue;
    }

    const hit = item.expect.filter((target) => out.includes(target));
    const missing = item.expect.filter((target) => !out.includes(target));
    const recall = hit.length / item.expect.length;
    const ok = recall >= item.min - 1e-9;
    if (!ok) failures += 1;
    rows.push({ name: item.name, kind: "hit", ok, ms, kb, recall, min: item.min, hit, missing });
  }

  const recallCases = rows.filter((row) => row.kind === "hit");
  const meanRecall = recallCases.reduce((sum, row) => sum + row.recall, 0) / Math.max(1, recallCases.length);

  if (saveIndex >= 0 && process.argv[saveIndex + 1]) {
    writeFileSync(process.argv[saveIndex + 1], JSON.stringify({ root, meanRecall, failures, rows }, null, 2));
  }
  if (json) {
    console.log(JSON.stringify({ root, meanRecall, failures, rows }, null, 2));
  } else {
    console.log(`root: ${root}`);
    for (const row of rows) {
      if (row.kind === "miss") {
        console.log(`${row.ok ? "PASS" : "FAIL"}  [miss] ${row.name}  (${row.ms}ms)${row.ok ? "" : `  缺少: ${row.missing.join(", ")}`}`);
      } else {
        const pct = `${Math.round(row.recall * 100)}%`.padStart(4);
        console.log(`${row.ok ? "PASS" : "FAIL"}  ${pct}  ${row.name}  (${row.ms}ms, ${row.kb}KB)${row.missing.length ? `  未召回: ${row.missing.join(", ")}` : ""}`);
      }
    }
    console.log(`\nmean recall(hit 用例): ${(meanRecall * 100).toFixed(1)}%  failures: ${failures}`);
  }
  process.exit(failures ? 1 : 0);
}

await main();
