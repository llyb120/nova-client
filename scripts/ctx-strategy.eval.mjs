import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdir, mkdtemp, readdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, extname, join, relative } from "node:path";
import { promisify } from "node:util";

import { contextBundle } from "./ctx-core.mjs";

const execFileAsync = promisify(execFile);
const CODE_EXTENSIONS = new Set([".js", ".jsx", ".ts", ".tsx", ".mjs", ".cjs", ".rs"]);
const IGNORED_DIRECTORIES = new Set([".git", "node_modules", "target", "dist", "build"]);
const DEFAULT_MAX_BYTES = 24 * 1024;
const ANSI_ESCAPE = /\x1b\[[0-?]*[ -/]*[@-~]/g;

function argument(name, fallback = "") {
  const prefix = `${name}=`;
  const value = process.argv.find((item) => item.startsWith(prefix));
  return value ? value.slice(prefix.length) : fallback;
}

function hasArgument(name) {
  return process.argv.includes(name);
}

function noiseFiles(topic, count = 12) {
  return Object.fromEntries(Array.from({ length: count }, (_, index) => [
    `src/noise/${topic}-note-${index}.ts`,
    [
      `// ${topic} API error caller test migration note ${index}; intentionally irrelevant.`,
      `export function unrelated${topic}${index}() {`,
      `  return "NOISE_${topic.toUpperCase()}_${index}";`,
      "}",
    ].join("\n"),
  ]));
}

const scenarios = [
  {
    name: "typescript-api-error",
    keywords: ["processOrder"],
    task: "修改 processOrder 的错误处理；保持 submitOrder API 兼容，并检查直接调用方、错误边界和测试。",
    files: {
      "src/order/processOrder.ts": [
        "import { normalizeOrder } from './normalizeOrder';",
        "import { OrderValidationError } from './errors';",
        "export function processOrder(input) {",
        "  const normalized = normalizeOrder(input);",
        "  if (!normalized.ok) throw new OrderValidationError(normalized.reason);",
        "  return normalized.value;",
        "}",
      ].join("\n"),
      "src/order/normalizeOrder.ts": [
        "export function normalizeOrder(input) {",
        "  return input?.id ? { ok: true, value: input } : { ok: false, reason: 'missing id' };",
        "}",
      ].join("\n"),
      "src/order/errors.ts": "export class OrderValidationError extends Error {}\n",
      "src/api/submitOrder.ts": [
        "import { processOrder } from '../order/processOrder';",
        "export async function submitOrder(input) {",
        "  return fetch('/orders', { method: 'POST', body: JSON.stringify(processOrder(input)) });",
        "}",
      ].join("\n"),
      "src/ui/orderBoundary.ts": [
        "import { OrderValidationError } from '../order/errors';",
        "export function handleOrderError(error) {",
        "  if (error instanceof OrderValidationError) return 'invalid-order';",
        "  throw error;",
        "}",
      ].join("\n"),
      "src/order/processOrder.test.ts": [
        "import { processOrder } from './processOrder';",
        "export function rejectsMissingId() {",
        "  try { processOrder({}); } catch (error) { return error.name === 'OrderValidationError'; }",
        "}",
      ].join("\n"),
      ...noiseFiles("Order"),
    },
    expected: [
      ["src/order/processOrder.ts", "export function processOrder(input)"],
      ["src/order/normalizeOrder.ts", "export function normalizeOrder(input)"],
      ["src/order/errors.ts", "export class OrderValidationError"],
      ["src/api/submitOrder.ts", "export async function submitOrder(input)"],
      ["src/ui/orderBoundary.ts", "export function handleOrderError(error)"],
      ["src/order/processOrder.test.ts", "export function rejectsMissingId()"],
    ],
  },
  {
    name: "tauri-command-bridge",
    keywords: ["addCompare"],
    task: "修复 addCompare API 参数过滤；检查 TypeScript 调用方、Tauri command/注册、两端请求类型和测试。",
    files: {
      "src/api/compare.ts": [
        "import { invoke } from '@tauri-apps/api/core';",
        "import type { AddCompareRequest } from './types';",
        "export async function addCompare(request: AddCompareRequest) {",
        "  return invoke('add_compare', { request });",
        "}",
      ].join("\n"),
      "src/api/types.ts": "export interface AddCompareRequest { left: string; right: string; metadata?: string; }\n",
      "src/components/ComparePanel.ts": [
        "import { addCompare } from '../api/compare';",
        "export function submitComparison(left: string, right: string) {",
        "  return addCompare({ left, right });",
        "}",
      ].join("\n"),
      "src/api/compare.test.ts": [
        "import { addCompare } from './compare';",
        "export function sendsFilteredCompare() { return addCompare({ left: 'a', right: 'b' }); }",
      ].join("\n"),
      "src-tauri/src/commands/compare.rs": [
        "use crate::types::compare::AddCompareRequest;",
        "#[tauri::command]",
        "pub async fn add_compare(request: AddCompareRequest) -> Result<String, String> {",
        "    Ok(format!(\"{}:{}\", request.left, request.right))",
        "}",
      ].join("\n"),
      "src-tauri/src/types/compare.rs": [
        "#[derive(serde::Deserialize)]",
        "#[serde(rename_all = \"camelCase\")]",
        "pub struct AddCompareRequest {",
        "    pub left: String,",
        "    pub right: String,",
        "    pub metadata: Option<String>,",
        "}",
      ].join("\n"),
      "src-tauri/src/lib.rs": [
        "mod commands;",
        "mod types;",
        "pub fn run() {",
        "    tauri::Builder::default().invoke_handler(tauri::generate_handler![commands::compare::add_compare]);",
        "}",
      ].join("\n"),
      ...noiseFiles("Compare"),
    },
    expected: [
      ["src/api/compare.ts", "export async function addCompare(request"],
      ["src/api/types.ts", "export interface AddCompareRequest"],
      ["src/components/ComparePanel.ts", "export function submitComparison"],
      ["src/api/compare.test.ts", "export function sendsFilteredCompare"],
      ["src-tauri/src/commands/compare.rs", "pub async fn add_compare(request"],
      ["src-tauri/src/types/compare.rs", "pub struct AddCompareRequest"],
      ["src-tauri/src/lib.rs", "generate_handler![commands::compare::add_compare]"],
    ],
  },
  {
    name: "event-string-bridge",
    keywords: ["publishSyncFinished"],
    task: "修改 publishSyncFinished 事件载荷；检查 emit/listen 字符串桥、前端监听方、Rust 载荷类型和测试。",
    files: {
      "src/events/contracts.ts": [
        "export const SYNC_FINISHED = 'sync:finished';",
        "export interface SyncFinishedPayload { projectId: string; changed: number; }",
      ].join("\n"),
      "src/events/publish.ts": [
        "import { emit } from '@tauri-apps/api/event';",
        "import { SYNC_FINISHED, type SyncFinishedPayload } from './contracts';",
        "export function publishSyncFinished(payload: SyncFinishedPayload) {",
        "  return emit(SYNC_FINISHED, payload);",
        "}",
      ].join("\n"),
      "src/events/SyncToast.ts": [
        "import { listen } from '@tauri-apps/api/event';",
        "import { SYNC_FINISHED, type SyncFinishedPayload } from './contracts';",
        "export function watchSyncFinished(show) {",
        "  return listen<SyncFinishedPayload>(SYNC_FINISHED, event => show(event.payload.changed));",
        "}",
      ].join("\n"),
      "src/events/publish.test.ts": [
        "import { publishSyncFinished } from './publish';",
        "export function publishesCount() { return publishSyncFinished({ projectId: 'p', changed: 2 }); }",
      ].join("\n"),
      "src-tauri/src/sync/events.rs": [
        "use crate::sync::types::SyncFinishedPayload;",
        "pub fn emit_sync_finished(app: &tauri::AppHandle, payload: SyncFinishedPayload) {",
        "    app.emit(\"sync:finished\", payload).unwrap();",
        "}",
      ].join("\n"),
      "src-tauri/src/sync/types.rs": [
        "#[derive(Clone, serde::Serialize)]",
        "#[serde(rename_all = \"camelCase\")]",
        "pub struct SyncFinishedPayload { pub project_id: String, pub changed: usize }",
      ].join("\n"),
      ...noiseFiles("Sync"),
    },
    expected: [
      ["src/events/publish.ts", "export function publishSyncFinished"],
      ["src/events/contracts.ts", "export const SYNC_FINISHED"],
      ["src/events/SyncToast.ts", "export function watchSyncFinished"],
      ["src/events/publish.test.ts", "export function publishesCount"],
      ["src-tauri/src/sync/events.rs", "pub fn emit_sync_finished"],
      ["src-tauri/src/sync/types.rs", "pub struct SyncFinishedPayload"],
    ],
  },
];

async function fixture(files) {
  const root = await mkdtemp(join(tmpdir(), "nova-context-strategy-"));
  for (const [path, text] of Object.entries(files)) {
    const absolute = join(root, path);
    await mkdir(dirname(absolute), { recursive: true });
    await writeFile(absolute, text);
  }
  return root;
}

async function codePaths(root, directory = root) {
  const paths = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    if (entry.isDirectory() && IGNORED_DIRECTORIES.has(entry.name)) continue;
    const absolute = join(directory, entry.name);
    if (entry.isDirectory()) paths.push(...await codePaths(root, absolute));
    else if (entry.isFile() && CODE_EXTENSIONS.has(extname(entry.name))) {
      paths.push(relative(root, absolute).replaceAll("\\", "/"));
    }
  }
  return paths.sort();
}

function stripComments(text) {
  return text.replace(/\/\*[\s\S]*?\*\//g, " ").replace(/(^|\s)\/\/.*$/gm, "$1");
}

function matches(text, regex) {
  return [...text.matchAll(regex)].map((match) => match[1]).filter(Boolean);
}

function canonicalSymbol(value) {
  return String(value).replace(/[^A-Za-z0-9]/g, "").toLowerCase();
}

function recordFor(path, text) {
  const code = stripComments(text);
  const declarations = new Set([
    ...matches(code, /(?:export\s+)?(?:default\s+)?(?:async\s+)?(?:function|class|interface|type|enum|const|let|var)\s+([A-Za-z_$][\w$]*)/g),
    ...matches(code, /(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:fn|struct|enum|trait|type|const|static|mod)\s+([A-Za-z_][\w]*)/g),
  ]);
  const identifiers = new Set(code.match(/[A-Za-z_$][\w$]*/g) ?? []);
  const imported = new Set();
  for (const match of code.matchAll(/import\s+(?:type\s+)?\{([^}]+)\}\s+from/g)) {
    for (const item of match[1].split(",")) imported.add(item.trim().split(/\s+as\s+/)[0].replace(/^type\s+/, ""));
  }
  for (const match of code.matchAll(/\buse\s+[^;]+;/g)) {
    for (const name of match[0].match(/[A-Za-z_][\w]*/g) ?? []) imported.add(name);
  }
  const strings = new Set(matches(code, /["'`]([^"'`\r\n]{3,80})["'`]/g));
  return { path, text, code, declarations, identifiers, imported, strings };
}

async function repositoryRecords(root) {
  return Promise.all((await codePaths(root)).map(async (path) => recordFor(path, await readFile(join(root, path), "utf8"))));
}

function retrievalIntent(task) {
  const text = task.toLowerCase();
  const has = (...terms) => terms.some((term) => text.includes(term));
  return {
    callers: has("调用方", "兼容", "api", "caller", "注册", "registration"),
    errors: has("错误", "失败", "异常", "error", "fail"),
    tests: has("测试", "回归", "test", "spec"),
    bridges: has("tauri", "ipc", "command", "invoke", "事件", "event", "emit", "listen", "跨层", "桥", "注册"),
  };
}

function renderRecords(title, records, reasons = new Map(), maxBytes = DEFAULT_MAX_BYTES) {
  const lines = [`# ${title}`];
  for (const record of records) {
    const reason = [...(reasons.get(record.path) ?? [])].join(",") || "included";
    const block = `\n### ${record.path} [${reason}]\n${record.text}\n`;
    if (Buffer.byteLength(`${lines.join("\n")}${block}`, "utf8") > maxBytes) continue;
    lines.push(block);
  }
  return lines.join("\n");
}

async function exactTextContext(root, scenario) {
  const records = await repositoryRecords(root);
  const selected = records.filter((record) => scenario.keywords.some((keyword) => record.code.includes(keyword)));
  return renderRecords("EXACT TEXT CONTROL", selected);
}

async function fullRepositoryContext(root) {
  return renderRecords("FULL REPOSITORY CONTROL", await repositoryRecords(root));
}

function addReason(selected, record, reason) {
  if (!selected.has(record.path)) selected.set(record.path, new Set());
  selected.get(record.path).add(reason);
}

async function obligationGraphContext(root, scenario) {
  const records = await repositoryRecords(root);
  const definitions = new Map();
  for (const record of records) {
    for (const declaration of record.declarations) {
      const key = canonicalSymbol(declaration);
      if (!definitions.has(key)) definitions.set(key, []);
      definitions.get(key).push(record);
    }
  }

  const intent = retrievalIntent(scenario.task);
  const selected = new Map();
  const seedSymbols = new Set(scenario.keywords.map(canonicalSymbol));
  for (const symbol of seedSymbols) {
    for (const record of definitions.get(symbol) ?? []) addReason(selected, record, "exact-or-alias-seed");
  }

  for (let round = 0; round < 4; round += 1) {
    const before = selected.size;
    const chosen = records.filter((record) => selected.has(record.path));
    const chosenDeclarations = new Set(chosen.flatMap((record) => [...record.declarations]));
    const dependencySymbols = new Set(chosen.flatMap((record) => [
      ...record.imported,
      ...[...record.identifiers].filter((name) => /^[A-Z]/.test(name)),
    ]));

    for (const name of dependencySymbols) {
      for (const record of definitions.get(canonicalSymbol(name)) ?? []) addReason(selected, record, "local-dependency");
    }

    const callerSymbols = new Set([...seedSymbols]);
    if (intent.bridges) {
      for (const name of chosenDeclarations) callerSymbols.add(canonicalSymbol(name));
    }
    if (intent.callers || intent.bridges) {
      for (const record of records) {
        if (selected.has(record.path)) continue;
        if ([...record.identifiers].some((name) => callerSymbols.has(canonicalSymbol(name)))) {
          addReason(selected, record, "direct-caller-or-registration");
        }
      }
    }

    if (intent.errors) {
      const errorNames = new Set([...chosenDeclarations].filter((name) => /(?:Error|Failure)$/.test(name)));
      for (const record of records) {
        if ([...errorNames].some((name) => record.identifiers.has(name))) addReason(selected, record, "error-boundary");
      }
    }

    if (intent.tests) {
      for (const record of records) {
        if (!/(?:\.test\.|\.spec\.|\/tests?\/)/.test(record.path)) continue;
        if ([...record.identifiers].some((name) => callerSymbols.has(canonicalSymbol(name)))) {
          addReason(selected, record, "representative-test");
        }
      }
    }

    if (intent.bridges) {
      const bridgeStrings = new Set(chosen.flatMap((record) => [...record.strings]).filter((value) => /[:_/-]/.test(value)));
      for (const record of records) {
        if ([...record.strings].some((value) => bridgeStrings.has(value))) addReason(selected, record, "semantic-string-bridge");
      }
    }
    if (selected.size === before) break;
  }

  const roleOrder = ["exact-or-alias-seed", "local-dependency", "semantic-string-bridge", "direct-caller-or-registration", "error-boundary", "representative-test"];
  const selectedRecords = records
    .filter((record) => selected.has(record.path))
    .sort((left, right) => {
      const rank = (record) => Math.min(...[...selected.get(record.path)].map((reason) => roleOrder.indexOf(reason)));
      return rank(left) - rank(right) || left.path.localeCompare(right.path);
    });
  const coverage = selectedRecords.map((record) => `✓ ${record.path}: ${[...selected.get(record.path)].join(",")}`);
  return `${renderRecords("OBLIGATION GRAPH FIXED-POINT", selectedRecords, selected)}\n\n## COVERAGE\n${coverage.join("\n")}\n`;
}

const strategies = [
  {
    name: "exact-text",
    run: exactTextContext,
  },
  {
    name: "current-fast-context",
    run(root, scenario) {
      return contextBundle({
        keywords: scenario.keywords,
        task: scenario.task,
        budget: 800,
        maxBytes: DEFAULT_MAX_BYTES,
      }, root);
    },
  },
  {
    name: "obligation-graph",
    run: obligationGraphContext,
  },
  {
    name: "full-repository",
    run: fullRepositoryContext,
  },
];

function selectedScenarios() {
  const requested = argument("--scenario");
  if (!requested) return scenarios;
  const selected = scenarios.filter((scenario) => scenario.name === requested);
  if (!selected.length) throw new Error(`unknown scenario: ${requested}`);
  return selected;
}

function contextMetrics(context, scenario) {
  const found = scenario.expected.filter(([, marker]) => context.includes(marker));
  const noise = Object.values(scenario.files).filter((text) => {
    const marker = text.match(/NOISE_[A-Z]+_\d+/)?.[0];
    return marker && context.includes(marker);
  });
  const bytes = Buffer.byteLength(context, "utf8");
  return {
    recall: found.length / scenario.expected.length,
    noise: noise.length,
    bytes,
    estimatedTokens: Math.ceil(bytes / 4),
  };
}

function normalizePath(path) {
  return String(path).trim().replaceAll("\\", "/").replace(/^\.\//, "");
}

function parseAgentFiles(stdout) {
  const text = stdout.replace(ANSI_ESCAPE, "").trim();
  const candidates = [
    ...[...text.matchAll(/```(?:json)?\s*([\s\S]*?)```/gi)].map((match) => match[1]),
    text,
  ];
  const first = text.indexOf("{");
  const last = text.lastIndexOf("}");
  if (first >= 0 && last > first) candidates.push(text.slice(first, last + 1));
  for (const candidate of candidates) {
    try {
      const parsed = JSON.parse(candidate.trim());
      if (Array.isArray(parsed?.files)) return [...new Set(parsed.files.map(normalizePath).filter(Boolean))];
    } catch {
      // Try next representation.
    }
  }
  throw new Error(`Devin did not return parseable JSON: ${text.slice(0, 300)}`);
}

function agentMetrics(files, scenario) {
  const expected = new Set(scenario.expected.map(([path]) => path));
  const known = new Set(Object.keys(scenario.files));
  const selected = new Set(files.filter((path) => known.has(path)));
  const truePositive = [...selected].filter((path) => expected.has(path)).length;
  const recall = truePositive / expected.size;
  const precision = selected.size ? truePositive / selected.size : 0;
  const f1 = precision + recall ? (2 * precision * recall) / (precision + recall) : 0;
  return { recall, precision, f1, files: [...selected] };
}

function agentPrompt(scenario, strategy, context) {
  return [
    "You evaluate whether one-shot code retrieval supplied enough context.",
    "Repository filesystem is unavailable. Use only CONTEXT. Do not call tools.",
    "List every repository file that a coding agent must inspect before safely implementing TASK:",
    "- target implementation",
    "- locally defined direct dependencies, request/payload types, and error types",
    "- direct callers, registrations, cross-language/string bridges, and representative tests when TASK asks for them",
    "Exclude docs, migration notes, generated files, and unrelated textual matches.",
    "Count a file only when CONTEXT contains an explicit `### relative/path` section with its relevant implementation body.",
    "An import, IMPACT/SIG/COVERAGE line, outline entry, or guessed path does not count as supplied implementation.",
    "Return strict JSON only: {\"files\":[\"relative/path\"]}",
    "Do not explain. Never infer missing files from imports or task wording.",
    "",
    `STRATEGY: ${strategy}`,
    `TASK: ${scenario.task}`,
    `KEYWORDS: ${scenario.keywords.join(", ")}`,
    "",
    "CONTEXT:",
    context,
  ].join("\n");
}

async function askDevin(prompt, model, cwd) {
  const promptPath = join(cwd, `prompt-${Date.now()}-${Math.random().toString(16).slice(2)}.txt`);
  await writeFile(promptPath, prompt);
  const started = performance.now();
  try {
    const { stdout, stderr } = await execFileAsync("devin", [
      "--model", model,
      "--permission-mode", "auto",
      "--prompt-file", promptPath,
      "--print",
    ], {
      cwd,
      encoding: "utf8",
      maxBuffer: 4 * 1024 * 1024,
      timeout: 180_000,
      windowsHide: true,
    });
    return { stdout, stderr, elapsedMs: performance.now() - started };
  } finally {
    await rm(promptPath, { force: true });
  }
}

function round(value, digits = 3) {
  return Number(value.toFixed(digits));
}

function aggregateRows(rows, live) {
  return strategies.map(({ name }) => {
    const selected = rows.filter((row) => row.strategy === name);
    const average = (field) => selected.reduce((sum, row) => sum + row[field], 0) / selected.length;
    const aggregate = {
      strategy: name,
      contextRecall: round(average("contextRecall")),
      noise: round(average("noise"), 1),
      bytes: Math.round(average("bytes")),
      estimatedTokens: Math.round(average("estimatedTokens")),
      retrievalMs: round(average("retrievalMs"), 1),
    };
    if (live) {
      aggregate.agentRecall = round(average("agentRecall"));
      aggregate.agentPrecision = round(average("agentPrecision"));
      aggregate.agentF1 = round(average("agentF1"));
      aggregate.modelMs = round(average("modelMs"), 1);
      aggregate.qualityPer1kTokens = round(aggregate.agentF1 * 1000 / Math.max(1, aggregate.estimatedTokens));
    }
    return aggregate;
  });
}

function winners(aggregates) {
  const byQuality = [...aggregates].sort((left, right) => right.agentF1 - left.agentF1 || left.estimatedTokens - right.estimatedTokens);
  const byEfficiency = [...aggregates].sort((left, right) => right.qualityPer1kTokens - left.qualityPer1kTokens);
  const bestQuality = byQuality[0].agentF1;
  const complete = aggregates.filter((item) => item.agentF1 >= 0.98 && item.contextRecall >= 0.98);
  const recommended = [...aggregates]
    .filter((item) => item.agentF1 >= bestQuality - 0.02 && item.contextRecall >= 0.98)
    .sort((left, right) => left.estimatedTokens - right.estimatedTokens || left.modelMs - right.modelMs)[0];
  const completeEfficiency = [...complete]
    .sort((left, right) => right.qualityPer1kTokens - left.qualityPer1kTokens)[0];
  return {
    bestEffect: byQuality[0].strategy,
    bestRawTokenEfficiency: byEfficiency[0].strategy,
    bestCompleteEfficiency: completeEfficiency?.strategy ?? null,
    recommended: recommended?.strategy ?? byQuality[0].strategy,
    rule: "require context recall >= 0.98; highest mean agent F1; within 0.02 choose fewer context tokens",
  };
}

function printRows(rows, live) {
  const header = ["scenario", "strategy", "ctxRecall", "noise", "bytes", "retrievalMs"];
  if (live) header.push("agentRecall", "agentPrecision", "agentF1", "modelMs", "selectedFiles");
  process.stdout.write(`${header.join("\t")}\n`);
  for (const row of rows) {
    const values = [row.scenario, row.strategy, row.contextRecall, row.noise, row.bytes, row.retrievalMs];
    if (live) values.push(row.agentRecall, row.agentPrecision, row.agentF1, row.modelMs, row.selectedFiles.join(","));
    process.stdout.write(`${values.join("\t")}\n`);
  }
}

const live = hasArgument("--live");
const model = argument("--model", "swe-1-6");
const repeats = Math.max(1, Number.parseInt(argument("--repeats", "1"), 10) || 1);
const rows = [];
const modelCwd = live ? await mkdtemp(join(tmpdir(), "nova-context-devin-")) : null;

try {
  for (const scenario of selectedScenarios()) {
    const root = await fixture(scenario.files);
    try {
      for (const strategy of strategies) {
        const started = performance.now();
        const context = await strategy.run(root, scenario);
        const retrievalMs = performance.now() - started;
        const metrics = contextMetrics(context, scenario);
        assert.ok(metrics.bytes <= DEFAULT_MAX_BYTES, `${scenario.name}/${strategy.name} exceeded context budget`);

        if (!live) {
          rows.push({
            scenario: scenario.name,
            strategy: strategy.name,
            contextRecall: round(metrics.recall),
            noise: metrics.noise,
            bytes: metrics.bytes,
            estimatedTokens: metrics.estimatedTokens,
            retrievalMs: round(retrievalMs, 1),
          });
          continue;
        }

        for (let repeat = 0; repeat < repeats; repeat += 1) {
          let agent = { recall: 0, precision: 0, f1: 0, files: [] };
          let modelMs = 0;
          let modelError = "";
          try {
            const response = await askDevin(agentPrompt(scenario, strategy.name, context), model, modelCwd);
            modelMs = response.elapsedMs;
            agent = agentMetrics(parseAgentFiles(response.stdout), scenario);
          } catch (error) {
            modelError = error instanceof Error ? error.message : String(error);
          }
          rows.push({
            scenario: scenario.name,
            strategy: strategy.name,
            repeat,
            contextRecall: round(metrics.recall),
            noise: metrics.noise,
            bytes: metrics.bytes,
            estimatedTokens: metrics.estimatedTokens,
            retrievalMs: round(retrievalMs, 1),
            agentRecall: round(agent.recall),
            agentPrecision: round(agent.precision),
            agentF1: round(agent.f1),
            modelMs: round(modelMs, 1),
            selectedFiles: agent.files,
            modelError,
          });
        }
      }
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  }
} finally {
  if (modelCwd) await rm(modelCwd, { recursive: true, force: true });
}

const graphRows = rows.filter((row) => row.strategy === "obligation-graph");
assert.ok(graphRows.every((row) => row.contextRecall === 1), "obligation graph must close every fixture");
assert.ok(rows.some((row) => row.strategy === "exact-text" && row.contextRecall < 1), "exact-text control must expose a recall gap");
if (live) assert.ok(rows.every((row) => !row.modelError), "every Devin evaluation must complete and return strict JSON");

const aggregates = aggregateRows(rows, live);
const report = {
  model: live ? model : null,
  repeats: live ? repeats : 0,
  rows,
  aggregates,
  winners: live ? winners(aggregates) : null,
};

if (hasArgument("--json")) {
  process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
} else {
  printRows(rows, live);
  process.stdout.write(`\nAGGREGATE\n${JSON.stringify(aggregates, null, 2)}\n`);
  if (live) process.stdout.write(`\nWINNERS\n${JSON.stringify(report.winners, null, 2)}\n`);
}
