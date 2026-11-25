import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import ts from "typescript";

const js = ts.transpileModule(
  readFileSync(new URL("../src/antigravityQuota.ts", import.meta.url), "utf8"),
  { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } },
).outputText;
const exportsObj = {};
new Function("exports", js)(exportsObj);
const {
  matchAntigravityQuotaGroup,
  pickQuotaBuckets,
  quotaTone,
  formatQuotaResetCountdown,
  formatQuotaUpdatedAgo,
  shortQuotaGroupName,
} = exportsObj;

const SAMPLE_SNAPSHOT = {
  accountId: "acc-1",
  fetchedAt: 1_700_000_000_000,
  groups: [
    {
      name: "Gemini Models",
      description:
        "Models within this group: Gemini 4 Neon, Gemini 4 Carbon, Orchestrator (Neon), Gemini 3.8 Flash",
      buckets: [
        { id: "gemini-weekly", name: "Weekly Limit Remaining", window: "weekly", remainingFraction: 0.48, remainingPercent: 48, resetTime: 1_700_500_000_000 },
        { id: "gemini-5h", name: "Five Hour Limit Remaining", window: "5h", remainingFraction: 0.97, remainingPercent: 97, resetTime: 1_700_010_000_000 },
      ],
    },
    {
      name: "Claude and GPT models",
      description: "Models within this group: Claude Opus 5.5, Claude Sonnet 5.5, GPT-OSS 120B",
      buckets: [
        { id: "3p-weekly", name: "Weekly Limit Remaining", window: "weekly", remainingFraction: 1, remainingPercent: 100, resetTime: null },
        { id: "3p-5h", name: "Five Hour Limit Remaining", window: "5h", remainingFraction: 0.2, remainingPercent: 20, resetTime: 1_700_003_600_000 },
      ],
    },
  ],
};

test("matchAntigravityQuotaGroup matches Gemini and Claude/GPT models accurately", () => {
  assert.equal(
    matchAntigravityQuotaGroup(SAMPLE_SNAPSHOT, "gemini-4-neon-high", "Gemini 4 Neon (High)")?.name,
    "Gemini Models",
  );
  assert.equal(
    matchAntigravityQuotaGroup(SAMPLE_SNAPSHOT, "claude-opus-5-5-high", "Claude Opus 5.5 (High)")?.name,
    "Claude and GPT models",
  );
  assert.equal(
    matchAntigravityQuotaGroup(SAMPLE_SNAPSHOT, "gpt-oss-120b-medium")?.name,
    "Claude and GPT models",
  );
  assert.equal(
    matchAntigravityQuotaGroup(SAMPLE_SNAPSHOT, "")?.name,
    "Gemini Models",
  );
  assert.equal(matchAntigravityQuotaGroup(null, "gemini-4-neon-high"), null);
});

test("pickQuotaBuckets extracts 5h and weekly buckets", () => {
  const { fiveHour, weekly } = pickQuotaBuckets(SAMPLE_SNAPSHOT.groups[0]);
  assert.equal(fiveHour?.remainingPercent, 97);
  assert.equal(weekly?.remainingPercent, 48);
  assert.deepEqual(pickQuotaBuckets(null), { fiveHour: null, weekly: null });
});

test("quotaTone maps percentages to ok, warn, and low", () => {
  assert.equal(quotaTone(100), "ok");
  assert.equal(quotaTone(31), "ok");
  assert.equal(quotaTone(30), "warn");
  assert.equal(quotaTone(11), "warn");
  assert.equal(quotaTone(10), "low");
  assert.equal(quotaTone(0), "low");
});

test("formatQuotaResetCountdown formats remaining durations and ignores empty/epoch-0", () => {
  const now = 1_700_000_000_000;
  assert.equal(formatQuotaResetCountdown(null, now), null);
  assert.equal(formatQuotaResetCountdown(0, now), null);
  assert.equal(formatQuotaResetCountdown(now - 1000, now), "即将重置");
  assert.equal(formatQuotaResetCountdown(now + 15 * 60_000, now), "15m");
  assert.equal(formatQuotaResetCountdown(now + (4 * 60 + 55) * 60_000, now), "4h55m");
  assert.equal(formatQuotaResetCountdown(now + 26 * 3_600_000, now), "1天2h");
  assert.equal(formatQuotaResetCountdown(now + 5 * 24 * 3_600_000, now), "5天");
});

test("formatQuotaUpdatedAgo and shortQuotaGroupName format cleanly", () => {
  const now = 1_700_000_000_000;
  assert.equal(formatQuotaUpdatedAgo(now - 15_000, now), "刚刚");
  assert.equal(formatQuotaUpdatedAgo(now - 5 * 60_000, now), "5分钟前");
  assert.equal(formatQuotaUpdatedAgo(now - 2 * 3_600_000, now), "2小时前");
  assert.equal(shortQuotaGroupName("Gemini Models"), "Gemini");
  assert.equal(shortQuotaGroupName("Claude and GPT models"), "Claude / GPT");
  assert.equal(shortQuotaGroupName("Custom Pool"), "Custom Pool");
});
