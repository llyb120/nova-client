import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { copyFile, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, extname, join } from "node:path";
import { promisify } from "node:util";

import { contextBundle } from "./ctx-core.mjs";

const execFileAsync = promisify(execFile);
const DEFAULT_MODEL = "swe-1-6";
const MAX_BYTES = 32 * 1024;
const TASK = "修复 addCompare 接口参数过滤问题；定位真实实现、参数类型、过滤逻辑、直接调用方和相关测试。";
const KEYWORDS = ["addCompare"];
const SOURCE_EXTENSIONS = new Set([
  ".c", ".cc", ".cpp", ".cs", ".go", ".h", ".hpp", ".java", ".js", ".jsx", ".kt", ".kts",
  ".lua", ".mjs", ".php", ".py", ".rb", ".rs", ".scala", ".swift", ".ts", ".tsx", ".vue",
]);
const ANSI_ESCAPE = /\x1b\[[0-?]*[ -/]*[@-~]/g;
const NON_PRODUCTION_SOURCE = /(?:^|\/)(?:docs?|examples?|fixtures?)\/|(?:^|\/)readme(?:\.|$)|\.(?:test|spec|eval|bench)\.[^.]+$|\/(?:__tests__|tests?|benches?)\//i;

function argument(name, fallback = "") {
  const prefix = `${name}=`;
  const value = process.argv.find((item) => item.startsWith(prefix));
  return value ? value.slice(prefix.length) : fallback;
}

function occurrences(text, pattern) {
  return text.split(pattern).length - 1;
}

async function trackedSourceSnapshot(repositoryRoot) {
  const { stdout: revision } = await execFileAsync("git", ["rev-parse", "--short", "HEAD"], {
    cwd: repositoryRoot,
    encoding: "utf8",
    windowsHide: true,
  });
  const { stdout } = await execFileAsync("git", ["ls-files", "-z"], {
    cwd: repositoryRoot,
    encoding: "utf8",
    maxBuffer: 16 * 1024 * 1024,
    windowsHide: true,
  });
  const paths = stdout.split("\0").filter((path) => SOURCE_EXTENSIONS.has(extname(path).toLowerCase()));
  const root = await mkdtemp(join(tmpdir(), "nova-addcompare-real-"));
  await Promise.all(paths.map(async (path) => {
    const destination = join(root, path);
    await mkdir(dirname(destination), { recursive: true });
    await copyFile(join(repositoryRoot, path), destination);
  }));
  return { root, revision: revision.trim(), paths };
}

async function exactHitPaths(root, paths) {
  const hits = [];
  for (const path of paths) {
    if (NON_PRODUCTION_SOURCE.test(path)) continue;
    if ((await readFile(join(root, path), "utf8")).includes("addCompare")) hits.push(path);
  }
  return hits;
}

function outputMetrics(output) {
  const bytes = Buffer.byteLength(output, "utf8");
  return {
    bytes,
    estimatedTokens: Math.ceil(bytes / 4),
    files: occurrences(output, "\n### "),
    blocks: occurrences(output, "\n@@ "),
    impactRows: output.includes("\n## IMPACT")
      ? output.slice(output.indexOf("\n## IMPACT")).split("\n").filter((line) => /^[^#\s].*:\d+\s/.test(line)).length
      : 0,
  };
}

async function currentFastContext(root) {
  return contextBundle({
    keywords: KEYWORDS,
    task: TASK,
    budget: 600,
    maxBytes: MAX_BYTES,
  }, root);
}

async function compactMissContext(root) {
  const evidence = await contextBundle({
    keywords: KEYWORDS,
    budget: 200,
    maxBytes: 4 * 1024,
  }, root);
  assert.ok(!evidence.includes("目标定义: 已闭合"), "exact addCompare lookup must not close a target in real tracked source");
  return [
    "# CTX MISS",
    `query: ${KEYWORDS.join(",")}`,
    `task: ${TASK}`,
    "status: no production definition or reference",
    "evidence: exact symbol search found 0 occurrences in tracked source",
    "checked: exact symbol, definitions/references, repository code index",
    "fallback: disabled; natural-language task terms cannot establish an addCompare edit target",
    "next: request the missing attachment/source repository or corrected symbol/path",
  ].join("\n");
}

async function broadFallbackContext(root) {
  return contextBundle({
    keywords: KEYWORDS,
    task: TASK,
    budget: 1_200,
    maxBytes: 64 * 1024,
  }, root);
}

const strategies = [
  ["current-fast-context", currentFastContext],
  ["obligation-graph-compact-miss", compactMissContext],
  ["broad-fallback-64k", broadFallbackContext],
];

function prompt(strategy, context) {
  return [
    "Evaluate a real repository retrieval result for a requested code change.",
    "Use only CONTEXT. Repository filesystem and tools are unavailable.",
    "Decide whether addCompare has an actionable production implementation in supplied context.",
    "If no production definition/reference exists, action must be ask_user and files must be empty.",
    "Do not treat benchmark fixtures, tests of retrieval tooling, documentation, SIG, IMPACT, or outlines as production implementation.",
    "Return strict JSON only:",
    '{"action":"edit|ask_user","targetFound":true|false,"files":["relative/path"],"decisionConfidence":0.0}',
    "",
    `STRATEGY: ${strategy}`,
    `TASK: ${TASK}`,
    "CONTEXT:",
    context,
  ].join("\n");
}

function parseJson(stdout) {
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
      if (parsed && typeof parsed === "object") return parsed;
    } catch {
      // Try next representation.
    }
  }
  throw new Error(`Devin did not return strict JSON: ${text.slice(0, 300)}`);
}

async function askDevin(cwd, model, strategy, context) {
  const promptPath = join(cwd, `prompt-${strategy}-${Date.now()}.txt`);
  await writeFile(promptPath, prompt(strategy, context));
  const started = performance.now();
  try {
    const { stdout } = await execFileAsync("devin", [
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
    return { decision: parseJson(stdout), elapsedMs: performance.now() - started };
  } finally {
    await rm(promptPath, { force: true });
  }
}

function scoreDecision(decision) {
  const files = Array.isArray(decision.files) ? decision.files.map(String) : [];
  const correctAction = decision.action === "ask_user";
  const correctTarget = decision.targetFound === false;
  const correctFiles = files.length === 0;
  return {
    correct: correctAction && correctTarget && correctFiles,
    correctAction,
    correctTarget,
    correctFiles,
    files,
    decisionConfidence: Number(decision.decisionConfidence) || 0,
  };
}

function round(value, digits = 1) {
  return Number(value.toFixed(digits));
}

const live = process.argv.includes("--live");
const model = argument("--model", DEFAULT_MODEL);
const repeats = Math.max(1, Number.parseInt(argument("--repeats", "1"), 10) || 1);
const repositoryRoot = process.cwd();
const snapshot = await trackedSourceSnapshot(repositoryRoot);
const exactHits = await exactHitPaths(snapshot.root, snapshot.paths);
const rows = [];
const contexts = [];

try {
  assert.deepEqual(exactHits, [], "real tracked source must not contain addCompare");
  for (const [strategy, run] of strategies) {
    const started = performance.now();
    const context = await run(snapshot.root);
    const retrievalMs = performance.now() - started;
    contexts.push({ strategy, context });
    const metrics = outputMetrics(context);
    assert.ok(metrics.bytes <= (strategy === "broad-fallback-64k" ? 64 * 1024 : MAX_BYTES));
    rows.push({
      strategy,
      retrievalMs: round(retrievalMs),
      ...metrics,
      declaresMiss: context.includes("no production definition or reference"),
    });
  }

  if (live) {
    const cwd = await mkdtemp(join(tmpdir(), "nova-addcompare-devin-"));
    try {
      for (const item of contexts) {
        const row = rows.find((candidate) => candidate.strategy === item.strategy);
        row.runs = [];
        for (let repeat = 0; repeat < repeats; repeat += 1) {
          const { decision, elapsedMs } = await askDevin(cwd, model, item.strategy, item.context);
          row.runs.push({ repeat, modelMs: round(elapsedMs), decision, ...scoreDecision(decision) });
        }
        row.accuracy = row.runs.filter((run) => run.correct).length / row.runs.length;
        row.averageModelMs = round(row.runs.reduce((sum, run) => sum + run.modelMs, 0) / row.runs.length);
      }
    } finally {
      await rm(cwd, { recursive: true, force: true });
    }
  }
} finally {
  await rm(snapshot.root, { recursive: true, force: true });
}

const recommended = live
  ? [...rows].sort((left, right) => right.accuracy - left.accuracy || left.estimatedTokens - right.estimatedTokens || left.retrievalMs - right.retrievalMs)[0].strategy
  : null;
const report = {
  case: "real repository addCompare miss",
  root: repositoryRoot,
  revision: snapshot.revision,
  truth: {
    productionTargetExists: false,
    exactRepositoryHits: exactHits,
    sourceScope: `${snapshot.paths.length} git-tracked source files; benchmark working-tree files excluded`,
    correctAction: "ask_user",
  },
  model: live ? model : null,
  repeats: live ? repeats : 0,
  rows,
  recommended,
};

process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
