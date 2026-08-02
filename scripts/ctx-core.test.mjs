import assert from "node:assert/strict";
import test from "node:test";
import { contextBundle, findSymbols, resolveRgBin } from "./ctx-core.mjs";

test("contextBundle rejects empty keywords", async () => {
  assert.equal(await contextBundle({ keywords: [] }), "错误: keywords 不能为空");
});

test("resolveRgBin prefers system or packaged rg when available", () => {
  const bin = resolveRgBin();
  if (bin) assert.match(String(bin), /rg/);
});

test("search falls back to system grep when forced", async () => {
  const prev = process.env.CTX_SEARCH_ENGINE;
  process.env.CTX_SEARCH_ENGINE = "grep";
  try {
    const out = await findSymbols({ names: ["contextBundle"] });
    assert.match(out, /## contextBundle/);
    assert.match(out, /scripts\/ctx-core\.mjs/);
  } finally {
    if (prev === undefined) delete process.env.CTX_SEARCH_ENGINE;
    else process.env.CTX_SEARCH_ENGINE = prev;
  }
});

test("contextBundle edit pack includes coverage and definition body", async () => {
  const out = await contextBundle({
    keywords: ["contextBundle", "mapPool"],
    intent: "edit",
    pathHints: ["scripts/"],
  });
  assert.match(out, /^# fast_context/m);
  assert.match(out, /chars: \d+\/(?:32000|40000|48000)/);
  assert.match(out, /## coverage/);
  assert.match(out, /## next_reads/);
  assert.match(out, /## rules/);
  assert.match(out, /scripts\/ctx-core\.mjs/);
  assert.match(out, /function contextBundle|async function contextBundle/);
  assert.match(out, /function mapPool|async function mapPool/);
  assert.ok(out.length > 4000);
  assert.ok(out.length <= 80_000);
});

test("explicit maxChars bypasses adaptive expansion", async () => {
  const out = await contextBundle({
    keywords: ["fast_context", "next_reads", "coverage", "maxChars"],
    intent: "edit",
    maxChars: 18_000,
  });
  assert.match(out, /chars: \d+\/18000/);
  assert.ok(out.length <= 18_000);
});

test("contextBundle locate stays outline-focused", async () => {
  const out = await contextBundle({ keywords: ["contextBundle"], intent: "locate" });
  assert.match(out, /intent: locate/);
  assert.match(out, /OUTLINE:/);
  assert.ok(out.length < 20_000);
});

test("findSymbols runs keywords in parallel shape", async () => {
  const out = await findSymbols({ names: ["contextBundle", "findSymbols"] });
  assert.match(out, /## contextBundle/);
  assert.match(out, /## findSymbols/);
  assert.match(out, /scripts\/ctx-core\.mjs/);
});

test("legacy budget maps without throwing", async () => {
  const out = await contextBundle({
    keywords: ["repoRoot"],
    budget: 200,
    ctx: 8,
    maxFiles: 4,
  });
  assert.match(out, /# fast_context/);
});
