// Deterministic repository-scale A/B for NOVA_CTX_SCALE_OPT.
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { createRequire } from "node:module";

const ROOT = join(tmpdir(), "nova-fast-context-scale-ab");
const ADDON = join(import.meta.dirname, "..", "src-tauri", "resources", "nova-tools-napi.node");
const SIZES = [1000, 4000, 8000];
const REPEATS = 5;

function fixture(size) {
  const root = join(ROOT, String(size));
  if (existsSync(join(root, ".ready"))) return root;
  rmSync(root, { recursive: true, force: true });
  mkdirSync(join(root, "src", "feature"), { recursive: true });
  mkdirSync(join(root, "src", "noise"), { recursive: true });
  writeFileSync(join(root, "src", "feature", "target.ts"), "export function scaleTarget(value: number) {\n  return value + 1;\n}\n");
  writeFileSync(join(root, "src", "feature", "caller.ts"), "import { scaleTarget } from './target';\nexport function scaleCaller() {\n  return scaleTarget(41);\n}\n");
  const featureCount = Math.max(200, Math.floor(size * 0.2));
  for (let i = 2; i < size; i++) {
    const dir = i < featureCount ? "feature" : "noise";
    const body = dir === "feature"
      ? `import { scaleTarget } from './target';\nexport function feature_${i}() { return scaleTarget(${i}); }\n`
      : `export function unrelated_${i}(value: number) { return value * ${i}; }\n`;
    writeFileSync(join(root, "src", dir, `file_${String(i).padStart(5, "0")}.ts`), body);
  }
  writeFileSync(join(root, ".ready"), "ok");
  return root;
}

if (process.argv[2] === "--child") {
  const [, , , root, arm] = process.argv;
  process.env.NOVA_CTX_SCALE_OPT = arm === "B" ? "1" : "0";
  process.env.NOVA_DATA_DIR = join(root, `.nova-${arm}`);
  const native = createRequire(import.meta.url)(ADDON);
  const params = { keywords: ["scaleTarget"], task: "分析 scaleTarget 的实现、调用方和依赖", budget: 600, maxBytes: 32768 };
  const elapsed = [];
  let output = "";
  for (let i = 0; i < REPEATS + 1; i++) {
    const started = performance.now();
    output = native.fastContext(root, params);
    const ms = performance.now() - started;
    if (i > 0) elapsed.push(ms);
  }
  process.stdout.write(JSON.stringify({ arm, elapsed, bytes: Buffer.byteLength(output), hasTarget: output.includes("scaleTarget"), hasCaller: output.includes("scaleCaller") }));
  process.exit(0);
}

rmSync(ROOT, { recursive: true, force: true });
const rows = [];
for (const size of SIZES) {
  const root = fixture(size);
  const pair = {};
  for (const arm of ["A", "B"]) {
    const result = spawnSync(process.execPath, [import.meta.filename, "--child", root, arm], { encoding: "utf8", timeout: 180000 });
    if (result.status !== 0) throw new Error(`${arm}/${size}: ${result.stderr || result.stdout}`);
    pair[arm] = JSON.parse(result.stdout);
  }
  const median = (xs) => [...xs].sort((a,b) => a-b)[Math.floor(xs.length/2)];
  rows.push({ size, A: pair.A, B: pair.B, medianA: median(pair.A.elapsed), medianB: median(pair.B.elapsed) });
}
writeFileSync(join(import.meta.dirname, "fast-context-scale-ab.report.json"), JSON.stringify({ ranAt: new Date().toISOString(), repeats: REPEATS, rows }, null, 2));
console.log("files | A median | B median | delta");
for (const row of rows) console.log(`${row.size} | ${row.medianA.toFixed(1)}ms | ${row.medianB.toFixed(1)}ms | ${((row.medianB / row.medianA - 1) * 100).toFixed(1)}%`);
