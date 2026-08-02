import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";

import { contextBundle } from "./ctx-core.mjs";

async function fixture(files) {
  const dir = await mkdtemp(join(tmpdir(), "ctx-core-eval-"));
  for (const [rel, body] of Object.entries(files)) {
    const abs = join(dir, rel);
    await mkdir(dirname(abs), { recursive: true });
    await writeFile(abs, body);
  }
  const git = (...args) => execFileSync("git", args, { cwd: dir, stdio: "ignore" });
  git("init", "-q");
  git("config", "user.email", "context-eval@nova.local");
  git("config", "user.name", "Context Eval");
  git("add", "-A");
  git("commit", "-qm", "fixture");
  return dir;
}

function occurrences(text, pattern) {
  return text.split(pattern).length - 1;
}

function scoreOutput(output, expected, noise = []) {
  let earned = 0;
  let possible = 0;
  const missing = [];
  for (const item of expected) {
    possible += item.weight;
    if (output.includes(item.marker)) earned += item.weight;
    else missing.push(item.name);
  }
  const noiseHits = noise.filter((marker) => output.includes(marker));
  const bytes = Buffer.byteLength(output, "utf8");
  return {
    score: possible ? earned / possible : 1,
    earned,
    possible,
    missing,
    noiseHits,
    bytes,
    estimatedTokens: Math.ceil(bytes / 4),
    scorePerKb: bytes ? earned / (bytes / 1024) : 0,
    sigs: occurrences(output, "\n## SIG"),
    impacts: occurrences(output, "\n## IMPACT"),
  };
}

function validateContract(output, args) {
  const hard = args.maxBytes ?? 32768;
  assert.ok(Buffer.byteLength(output, "utf8") <= hard, `output exceeds maxBytes=${hard}`);
  assert.ok(!output.includes("partial"), "output must not contain partial units");
  assert.ok(!output.includes("[truncated]"), "output must not contain truncated units");
}

const longTargetFiller = Array.from({ length: 145 }, (_, i) => `export const TARGET_PAD_${i} = ${i};`).join("\n");
const longNoiseFiller = Array.from({ length: 120 }, (_, i) => `export const NOISE_PAD_${i} = ${i};`).join("\n");

const scenarios = [
  {
    name: "cross-file-change",
    files: {
      "src/order/processOrder.ts": [
        "import { normalizeOrder } from './normalizeOrder';",
        "import { OrderValidationError } from './errors';",
        "",
        "export function processOrder(input) {",
        "  const normalized = normalizeOrder(input);",
        "  if (!normalized.ok) throw new OrderValidationError(normalized.reason);",
        "  return normalized.value;",
        "}",
        "",
        longTargetFiller,
      ].join("\n"),
      "src/order/normalizeOrder.ts": [
        "export function normalizeOrder(input) {",
        "  return input?.id ? { ok: true, value: input } : { ok: false, reason: 'missing id' };",
        "}",
      ].join("\n"),
      "src/order/errors.ts": [
        "export class OrderValidationError extends Error {",
        "  constructor(message) { super(message); this.name = 'OrderValidationError'; }",
        "}",
      ].join("\n"),
      "src/api/submitOrder.ts": [
        "import { processOrder } from '../order/processOrder';",
        "export async function submitOrder(input) {",
        "  const order = processOrder(input);",
        "  return fetch('/orders', { method: 'POST', body: JSON.stringify(order) });",
        "}",
      ].join("\n"),
      "src/order/processOrder.test.ts": [
        "import { processOrder } from './processOrder';",
        "export function rejectsMissingId() {",
        "  try { processOrder({}); } catch (error) { return error.name === 'OrderValidationError'; }",
        "}",
      ].join("\n"),
      "src/noise/orderNotes.ts": [
        "// processOrder migration notes; intentionally irrelevant text hit",
        "export function irrelevantOrderNotes() { return 'NOISE_ONLY_MARKER'; }",
        longNoiseFiller,
      ].join("\n"),
    },
    expected: [
      { name: "edit-unit", marker: "export function processOrder(input)", weight: 5 },
      { name: "direct-dependency", marker: "export function normalizeOrder(input)", weight: 3 },
      { name: "error-type", marker: "export class OrderValidationError", weight: 3 },
      { name: "caller", marker: "export async function submitOrder(input)", weight: 2 },
      { name: "test", marker: "export function rejectsMissingId()", weight: 2 },
    ],
    noise: ["NOISE_ONLY_MARKER"],
    variants: [
      { name: "keyword-only", args: { keywords: ["processOrder"] } },
      {
        name: "rich-task",
        args: {
          keywords: ["processOrder", "OrderValidationError"],
          task: "修改 processOrder：normalizeOrder 失败时返回 OrderValidationError；保持 submitOrder API，并检查 rejectsMissingId 测试",
        },
      },
      {
        name: "guided-files",
        args: {
          keywords: ["processOrder", "OrderValidationError"],
          task: "修改失败处理并保持 submitOrder API",
          files: ["src/order/processOrder.ts", "src/api/submitOrder.ts", "src/order/processOrder.test.ts"],
        },
      },
      {
        name: "guided-64k",
        args: {
          keywords: ["processOrder", "OrderValidationError"],
          task: "修改失败处理并保持 submitOrder API",
          files: ["src/order/processOrder.ts", "src/api/submitOrder.ts", "src/order/processOrder.test.ts"],
          budget: 1200,
          maxBytes: 65536,
        },
      },
    ],
  },
  {
    name: "reverse-error-plan",
    files: {
      "src/auth/refreshSession.ts": [
        "import { SessionExpiredError } from './errors';",
        "export function refreshSession(token) {",
        "  if (!token) throw new SessionExpiredError('expired');",
        "  return token;",
        "}",
        longTargetFiller,
      ].join("\n"),
      "src/auth/errors.ts": "export class SessionExpiredError extends Error {}\n",
      "src/ui/sessionBoundary.ts": [
        "import { SessionExpiredError } from '../auth/errors';",
        "export function handleSessionFailure(error) {",
        "  if (error instanceof SessionExpiredError) return 'login';",
        "  throw error;",
        "}",
      ].join("\n"),
    },
    expected: [
      { name: "target", marker: "export function refreshSession(token)", weight: 5 },
      { name: "error-type", marker: "export class SessionExpiredError", weight: 2 },
      { name: "error-handler", marker: "export function handleSessionFailure(error)", weight: 4 },
    ],
    variants: [
      { name: "keyword-only", args: { keywords: ["refreshSession"] } },
      {
        name: "planned-errors",
        args: {
          keywords: ["refreshSession"],
          task: "修改 refreshSession 的失败和错误处理，保持会话过期行为",
        },
      },
    ],
  },
  {
    name: "case-mismatch",
    files: {
      "src/render.ts": [
        "export function RenderPipeline(node) {",
        "  return node ? String(node) : '';",
        "}",
      ].join("\n"),
    },
    expected: [{ name: "case-insensitive-definition", marker: "export function RenderPipeline(node)", weight: 5 }],
    variants: [
      { name: "lowercase-keyword", args: { keywords: ["renderpipeline"] } },
      { name: "exact-keyword", args: { keywords: ["RenderPipeline"] } },
      { name: "lowercase-with-file", args: { keywords: ["renderpipeline"], files: ["src/render.ts"] } },
    ],
  },
  {
    name: "budget-scaling",
    files: Object.fromEntries(Array.from({ length: 10 }, (_, i) => {
      const body = Array.from({ length: 42 }, (_, n) => `  const stage_${i}_${n} = ${n};`);
      return [`src/flow${i}.ts`, [`export function checkoutFlow${i}() {`, ...body, `  return 'FLOW_${i}_DONE';`, "}"].join("\n")];
    })),
    expected: Array.from({ length: 10 }, (_, i) => ({
      name: `flow-${i}`,
      marker: `FLOW_${i}_DONE`,
      weight: 1,
    })),
    variants: [
      { name: "8k", args: { keywords: ["checkoutFlow"], budget: 1200, maxBytes: 8192 } },
      { name: "32k", args: { keywords: ["checkoutFlow"], budget: 1200, maxBytes: 32768 } },
      { name: "64k", args: { keywords: ["checkoutFlow"], budget: 1200, maxBytes: 65536 } },
    ],
  },
];

const rows = [];
for (const scenario of scenarios) {
  const dir = await fixture(scenario.files);
  try {
    for (const variant of scenario.variants) {
      const started = performance.now();
      const output = await contextBundle(variant.args, dir);
      const elapsedMs = performance.now() - started;
      validateContract(output, variant.args);
      const metrics = scoreOutput(output, scenario.expected, scenario.noise);
      rows.push({
        scenario: scenario.name,
        variant: variant.name,
        score: Number(metrics.score.toFixed(3)),
        earned: `${metrics.earned}/${metrics.possible}`,
        bytes: metrics.bytes,
        estimatedTokens: metrics.estimatedTokens,
        scorePerKb: Number(metrics.scorePerKb.toFixed(3)),
        noise: metrics.noiseHits.length,
        missing: metrics.missing.join(",") || "-",
        elapsedMs: Number(elapsedMs.toFixed(1)),
      });
    }
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
}

if (process.argv.includes("--json")) {
  process.stdout.write(`${JSON.stringify(rows, null, 2)}\n`);
} else {
  process.stdout.write("scenario\tvariant\tscore\tearned\tbytes\t~tokens\tscore/KB\tnoise\tms\tmissing\n");
  for (const row of rows) {
    process.stdout.write([
      row.scenario,
      row.variant,
      row.score,
      row.earned,
      row.bytes,
      row.estimatedTokens,
      row.scorePerKb,
      row.noise,
      row.elapsedMs,
      row.missing,
    ].join("\t") + "\n");
  }
}
