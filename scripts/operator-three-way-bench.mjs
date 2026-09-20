import assert from "node:assert/strict";

export const strategies = ["legacy-main", "isolated-step", "operator"];
export const scenarios = [
  {name:"otp-cross-tool",steps:9,boundaries:[0,3,7],parentChars:42000,obs:[2200,1800,2500,1900,2100,1700,2400,1800,1600]},
  {name:"form",steps:8,boundaries:[0,3,6],parentChars:36000,obs:[2800,2300,2100,1800,1900,1700,2100,1500]},
  {name:"canvas-fallback",steps:7,boundaries:[0,2,4,6],parentChars:39000,obs:[3200,4200,3900,3600,2900,2500,2200]},
  {name:"native-dialog",steps:6,boundaries:[0,2,5],parentChars:33000,obs:[2600,2400,2200,2100,1900,1600]},
  {name:"timeout-recovery",steps:8,boundaries:[0,2,5,7],parentChars:41000,obs:[2100,2200,2600,1800,2000,2300,1700,1500],unknownSideEffectAt:5},
  {name:"simple-click",steps:2,boundaries:[0,1],parentChars:30000,obs:[1500,1300]}
];

function decisions(strategy, scenario) {
  if (strategy === "operator") return new Set(scenario.boundaries);
  return new Set(Array.from({length:scenario.steps}, function(_,i){ return i; }));
}

export function simulate(strategy, scenario) {
  const decision = decisions(strategy, scenario);
  let transcript = strategy === "legacy-main" ? scenario.parentChars : 3800;
  let contextChars = 0;
  let peakChars = transcript;
  let toolCalls = 0;
  let unknownReplays = 0;
  for (let step=0; step<scenario.steps; step++) {
    toolCalls += 1;
    transcript += scenario.obs[step];
    if (decision.has(step)) {
      contextChars += transcript;
      peakChars = Math.max(peakChars, transcript);
    }
    if (strategy === "operator" && step < scenario.steps - 2) {
      transcript -= scenario.obs[step] - Math.min(scenario.obs[step], 1200);
    }
    if (scenario.unknownSideEffectAt === step && strategy !== "operator") {
      unknownReplays += 1;
      toolCalls += 1;
    }
  }
  const modelRounds = decision.size;
  const modeledMs = modelRounds * 850 + toolCalls * 280;
  return {strategy,scenario:scenario.name,modelRounds,toolCalls,contextChars,peakChars,modeledMs,unknownReplays};
}

export function runSuite() {
  const rows = [];
  for (const scenario of scenarios) {
    for (const strategy of strategies) rows.push(simulate(strategy,scenario));
  }
  const summary = strategies.map(function(strategy) {
    const selected = rows.filter(function(row){ return row.strategy === strategy; });
    const sum = function(key){ return selected.reduce(function(n,row){ return n + row[key]; },0); };
    return {
      strategy,
      modelRounds:sum("modelRounds"),
      toolCalls:sum("toolCalls"),
      contextChars:sum("contextChars"),
      peakChars:Math.max.apply(null,selected.map(function(row){ return row.peakChars; })),
      modeledMs:sum("modeledMs"),
      unknownReplays:sum("unknownReplays")
    };
  });
  return {rows,summary};
}

function delta(value, base) {
  return ((value / base - 1) * 100).toFixed(1) + "%";
}

export function markdownReport() {
  const summary = runSuite().summary;
  const base = summary[0];
  const isolated = summary[1];
  const optimized = summary[2];
  const lines = [
    "# Operator three-way replay benchmark",
    "",
    "Deterministic structural replay over six representative UI traces. It measures orchestration cost, not live-model accuracy or real desktop wall time.",
    "",
    "| strategy | model rounds | tool calls | accumulated context chars | peak context chars | modeled latency | unresolved side-effect replays |",
    "|---|---:|---:|---:|---:|---:|---:|"
  ];
  for (const row of summary) {
    lines.push("| " + row.strategy + " | " + row.modelRounds + " | " + row.toolCalls + " | " + row.contextChars + " | " + row.peakChars + " | " + row.modeledMs + " ms | " + row.unknownReplays + " |");
  }
  lines.push("");
  lines.push("- isolation-only vs legacy: model rounds " + delta(isolated.modelRounds,base.modelRounds) + ", context " + delta(isolated.contextChars,base.contextChars) + ".");
  lines.push("- optimized operator vs legacy: model rounds " + delta(optimized.modelRounds,base.modelRounds) + ", tool calls " + delta(optimized.toolCalls,base.toolCalls) + ", context " + delta(optimized.contextChars,base.contextChars) + ", modeled latency " + delta(optimized.modeledMs,base.modeledMs) + ".");
  lines.push("- optimized replay does not repeat an unresolved side-effect action; it requires outcome verification first.");
  return lines.join("\n") + "\n";
}

const suite = runSuite();
const by = Object.fromEntries(suite.summary.map(function(row){ return [row.strategy,row]; }));
assert.equal(by["legacy-main"].modelRounds,by["isolated-step"].modelRounds);
assert.ok(by["isolated-step"].contextChars < by["legacy-main"].contextChars);
assert.ok(by.operator.modelRounds < by["isolated-step"].modelRounds);
assert.ok(by.operator.contextChars < by["isolated-step"].contextChars);
assert.equal(by.operator.unknownReplays,0);

if (process.argv[1] && import.meta.url === new URL("file://" + process.argv[1]).href) {
  process.stdout.write(markdownReport());
}
