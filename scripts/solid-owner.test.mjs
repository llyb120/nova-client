import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const sidebar = readFileSync(new URL("../src/components/Sidebar.tsx", import.meta.url), "utf8");
const evidence = readFileSync(new URL("../src/components/EvidenceChainView.tsx", import.meta.url), "utf8");
const vite = readFileSync(new URL("../vite.config.ts", import.meta.url), "utf8");

test("dynamic list callbacks do not create unowned Solid computations", () => {
  assert.doesNotMatch(sidebar, /\{\(row\)\s*=>\s*ThreadRow\(/);
  assert.doesNotMatch(sidebar, /const rows = createMemo\(/);
  assert.doesNotMatch(evidence, /const timeline = createMemo/);
});

test("Solid dev HMR is disabled with the app's disabled Vite HMR", () => {
  assert.match(vite, /solid\(\{\s*hot:\s*false\s*\}\)/);
});
