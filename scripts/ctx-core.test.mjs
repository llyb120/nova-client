import assert from "node:assert/strict";
import test from "node:test";
import { contextBundle, findSymbols } from "./ctx-core.mjs";

test("contextBundle rejects empty keywords", async () => {
  assert.equal(await contextBundle({ keywords: [] }), "错误: keywords 不能为空");
});

test("contextBundle edit pack includes coverage and definition body", async () => {
  const out = await contextBundle({
    keywords: ["contextBundle", "mapPool"],
    intent: "edit",
    pathHints: ["scripts/"],
  });
  assert.match(out, /^# fast_context/m);
  assert.match(out, /## coverage/);
  assert.match(out, /## next_reads/);
  assert.match(out, /## rules/);
  assert.match(out, /scripts\/ctx-core\.mjs/);
  assert.match(out, /function contextBundle|async function contextBundle/);
  assert.match(out, /function mapPool|async function mapPool/);
  assert.ok(out.length > 4000);
  assert.ok(out.length <= 80_000);
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
