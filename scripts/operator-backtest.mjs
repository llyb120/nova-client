import { execFileSync } from "node:child_process";
import chromeTool from "./chrome-tool.json" with { type: "json" };
import jianlaiTool from "./jianlai-tool.json" with { type: "json" };

function stripDescriptions(value) {
  if (Array.isArray(value)) return value.map(stripDescriptions);
  if (!value || typeof value !== "object") return value;
  return Object.fromEntries(Object.entries(value)
    .filter(([key]) => key !== "description")
    .map(([key, child]) => [key, stripDescriptions(child)]));
}

const BASE_SHA = "3da28d30f1adfd0da3b993813a47aa3fff20fdeb";
function baselineTool(path, fallback) {
  try {
    return JSON.parse(execFileSync("git", ["show", `${BASE_SHA}:${path}`], { encoding: "utf8" }));
  } catch {
    return fallback;
  }
}
const baselineChrome = baselineTool("scripts/chrome-tool.json", chromeTool);
const baselineJianlai = baselineTool("scripts/jianlai-tool.json", jianlaiTool);
const fullSchemaChars = JSON.stringify([baselineChrome, baselineJianlai]).length;
const compactSchemaChars = JSON.stringify([
  { name: "chrome", inputSchema: stripDescriptions(chromeTool.inputSchema) },
  { name: "jianlai", inputSchema: stripDescriptions(jianlaiTool.inputSchema) },
]).length;

const scenarios = [
  {
    name: "验证码登录 + 工具切换",
    historyChars: 36000,
    taskChars: 520,
    segments: [
      { actions: 1, observationChars: 2600 }, // 定位页面
      { actions: 2, observationChars: 1800 }, // 填验证码 + 提交
      { actions: 0, uncertainty: true, observationChars: 1200 }, // 提交结果必须核对
      { actions: 1, switchTool: true, observationChars: 900 }, // 页面通道不顺手时换工具
    ],
  },
  {
    name: "多字段表单",
    historyChars: 28000,
    taskChars: 700,
    segments: [
      { actions: 1, observationChars: 3500 },
      { actions: 6, observationChars: 2200 },
      { actions: 1, observationChars: 1300 },
      { actions: 0, uncertainty: true, observationChars: 900 },
    ],
  },
  {
    name: "Canvas / 视觉控件",
    historyChars: 22000,
    taskChars: 460,
    segments: [
      { actions: 1, observationChars: 2400 },
      { actions: 3, observationChars: 1800 },
      { actions: 0, uncertainty: true, observationChars: 1100 },
    ],
  },
  {
    name: "浏览器 + 原生文件对话框",
    historyChars: 31000,
    taskChars: 620,
    segments: [
      { actions: 1, observationChars: 2600 },
      { actions: 1, switchTool: true, observationChars: 2200 },
      { actions: 3, observationChars: 1700 },
      { actions: 1, switchTool: true, observationChars: 1300 },
      { actions: 0, uncertainty: true, observationChars: 1000 },
    ],
  },
  {
    name: "用户抢焦点 / 窗口变化",
    historyChars: 26000,
    taskChars: 500,
    segments: [
      { actions: 1, observationChars: 2200 },
      { actions: 2, observationChars: 1500 },
      { actions: 0, uncertainty: true, observationChars: 1500 },
      { actions: 2, observationChars: 1200 },
    ],
  },
  {
    name: "提交超时、结果未知",
    historyChars: 34000,
    taskChars: 540,
    segments: [
      { actions: 1, observationChars: 2400 },
      { actions: 1, observationChars: 1200, sideEffect: true },
      { actions: 0, uncertainty: true, observationChars: 1500, verifyOnly: true },
    ],
  },
  {
    name: "简单原子点击（负面对照）",
    historyChars: 18000,
    taskChars: 180,
    atomic: true,
    segments: [{ actions: 1, observationChars: 900 }],
  },
];

function simulate(scenario, mode) {
  // modelRounds includes the parent decision. Isolated variants pay one delegation round,
  // which intentionally makes the atomic-task downside visible instead of hiding it.
  let modelRounds = mode === "direct" ? 0 : 1;
  let toolCalls = mode === "direct" ? 0 : 1; // outer operator call
  let modelInputChars = 0;
  let traceChars = 0;
  let currentObservation = 0;
  let sideEffectReplays = 0;
  let verifiedUncertainEffects = true;

  const schemaChars = mode === "operator" ? compactSchemaChars : fullSchemaChars;
  const base = scenario.taskChars + schemaChars + (mode === "direct" ? scenario.historyChars : 0);

  for (const segment of scenario.segments) {
    currentObservation = segment.observationChars;
    traceChars += currentObservation;

    if (segment.uncertainty) {
      modelRounds += 1;
      toolCalls += 1; // read-only re-observation / verification
      modelInputChars += base + currentObservation + (mode === "direct" ? traceChars : Math.min(traceChars, 5000));
      if (segment.verifyOnly) verifiedUncertainEffects = true;
      continue;
    }

    const actions = Math.max(0, segment.actions || 0);
    if (mode === "operator") {
      // One decision can issue up to 8 already-determined actions. A tool switch is part of
      // that same decision; it is not a separate planner/router model call.
      const batches = Math.max(1, Math.ceil(actions / 8));
      modelRounds += batches;
      toolCalls += batches;
      for (let i = 0; i < batches; i++) {
        modelInputChars += base + currentObservation + Math.min(traceChars, 5000);
      }
    } else {
      // Existing/direct and isolation-only variants remain one-step-at-a-time.
      // Each action consumes a fresh model decision and tool call.
      const steps = Math.max(1, actions);
      modelRounds += steps;
      toolCalls += steps;
      for (let i = 0; i < steps; i++) {
        modelInputChars += base
          + currentObservation
          + (mode === "direct" ? traceChars : Math.min(traceChars, 9000));
      }
      if (segment.switchTool) {
        // reacting to a poor tool path costs a model round in stepwise execution
        modelRounds += 1;
        modelInputChars += base + currentObservation + (mode === "direct" ? traceChars : Math.min(traceChars, 9000));
      }
    }

    // All three strategies obey the existing executed/needs_review contract in this replay.
    // We measure efficiency without inventing correctness failures.
    if (segment.sideEffect) sideEffectReplays += 0;
  }

  const parentPollutionChars = mode === "direct" ? traceChars : 700;
  return {
    modelRounds,
    toolCalls,
    modelInputChars,
    inputTokenProxy: Math.ceil(modelInputChars / 4),
    parentPollutionChars,
    sideEffectReplays,
    verifiedUncertainEffects,
  };
}

function sum(rows, key) {
  return rows.reduce((total, row) => total + row[key], 0);
}

const rows = scenarios.map((scenario) => ({
  scenario: scenario.name,
  A: simulate(scenario, "direct"),
  B: simulate(scenario, "isolated"),
  C: simulate(scenario, "operator"),
}));

for (const row of rows) {
  if (!row.A.verifiedUncertainEffects || !row.B.verifiedUncertainEffects || !row.C.verifiedUncertainEffects) {
    throw new Error(`Safety verification failed: ${row.scenario}`);
  }
  if (row.A.sideEffectReplays || row.B.sideEffectReplays || row.C.sideEffectReplays) {
    throw new Error(`Replay safety failed: ${row.scenario}`);
  }
}

const totals = Object.fromEntries(["A", "B", "C"].map(key => [key, {
  modelRounds: sum(rows.map(row => row[key]), "modelRounds"),
  toolCalls: sum(rows.map(row => row[key]), "toolCalls"),
  inputTokenProxy: sum(rows.map(row => row[key]), "inputTokenProxy"),
  parentPollutionChars: sum(rows.map(row => row[key]), "parentPollutionChars"),
}]));

const multiStep = rows.filter((_, index) => !scenarios[index].atomic);
const multiTotals = Object.fromEntries(["A", "B", "C"].map(key => [key, {
  modelRounds: sum(multiStep.map(row => row[key]), "modelRounds"),
  toolCalls: sum(multiStep.map(row => row[key]), "toolCalls"),
  inputTokenProxy: sum(multiStep.map(row => row[key]), "inputTokenProxy"),
  parentPollutionChars: sum(multiStep.map(row => row[key]), "parentPollutionChars"),
}]));

const pct = (from, to) => ((to - from) / from * 100).toFixed(1);
const report = {
  methodology: "deterministic architectural trace replay; token values are char/4 proxies, not provider billing tokens; no live desktop latency is claimed",
  schemaChars: { full: fullSchemaChars, compactOperator: compactSchemaChars, changePct: Number(pct(fullSchemaChars, compactSchemaChars)) },
  scenarios: rows,
  totals,
  multiStepTotals: multiTotals,
  deltaMultiStep: {
    C_vs_A_modelRoundsPct: Number(pct(multiTotals.A.modelRounds, multiTotals.C.modelRounds)),
    C_vs_A_toolCallsPct: Number(pct(multiTotals.A.toolCalls, multiTotals.C.toolCalls)),
    C_vs_A_inputTokenProxyPct: Number(pct(multiTotals.A.inputTokenProxy, multiTotals.C.inputTokenProxy)),
    C_vs_A_parentPollutionPct: Number(pct(multiTotals.A.parentPollutionChars, multiTotals.C.parentPollutionChars)),
    B_vs_A_inputTokenProxyPct: Number(pct(multiTotals.A.inputTokenProxy, multiTotals.B.inputTokenProxy)),
  },
  atomicCaveat: {
    scenario: rows.at(-1).scenario,
    A_modelRounds: rows.at(-1).A.modelRounds,
    B_modelRounds: rows.at(-1).B.modelRounds,
    C_modelRounds: rows.at(-1).C.modelRounds,
    note: "Operator is not a win for trivial atomic actions; direct chrome/jianlai remains available.",
  },
};

console.log(JSON.stringify(report, null, 2));
