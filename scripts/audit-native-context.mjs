import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { performance } from "node:perf_hooks";
import { codeMap, findSymbols } from "./ctx-core.mjs";
import { callNapiTool } from "./nova-napi-tools.mjs";

// fast_context 的唯一实现是 Rust native（JS 镜像已移除），本审计只校验：
//   1. native fast_context 输出确定性（同参两跑一致）与基本契约（EDIT/PROOF）；
//   2. find_symbols / code_map 的 JS ↔ native 输出逐字节一致。

async function fixture(files) {
  const root = await mkdtemp(join(tmpdir(), "nova-native-context-"));
  for (const [relative, content] of Object.entries(files)) {
    const absolute = join(root, relative);
    await mkdir(join(absolute, ".."), { recursive: true });
    await writeFile(absolute, content);
  }
  const git = (...args) => execFileSync("git", args, { cwd: root, stdio: "ignore", windowsHide: true });
  git("init", "-q");
  git("config", "user.email", "native-parity@nova.local");
  git("config", "user.name", "Nova Native Parity");
  git("add", "-A");
  git("commit", "-qm", "fixture");
  return root;
}

async function timed(run) {
  const start = performance.now();
  const value = await run();
  return { value, ms: performance.now() - start };
}

const filler = Array.from({ length: 160 }, (_, index) => `export const PAD_${index} = ${index};`).join("\n");
const cases = [
  {
    name: "definition and imported dependency",
    files: {
      "src/target.ts": `import { helperFn } from './helper';\n\nexport function targetFn(input: string) {\n  const cleaned = input.trim();\n  return helperFn(cleaned);\n}\n\n${filler}`,
      "src/helper.ts": "export function helperFn(value: string) {\n  return `${value}!`;\n}\n",
    },
    params: { keywords: ["targetFn"] },
  },
  {
    name: "hard byte budget",
    files: Object.fromEntries(Array.from({ length: 8 }, (_, file) => [
      `src/f${file}.ts`,
      `export function bounded${file}() {\n${Array.from({ length: 35 }, (_, line) => `  const value_${file}_${line} = ${line};`).join("\n")}\n  return 1;\n}`,
    ])),
    params: { keywords: ["bounded"], maxBytes: 8192 },
  },
];

let cold = 0;
let warm = 0;
for (const item of cases) {
  const root = await fixture(item.files);
  try {
    const first = await timed(() => callNapiTool("fast_context", root, item.params));
    const second = await timed(() => callNapiTool("fast_context", root, item.params));
    assert.equal(second.value, first.value, `${item.name}: deterministic output`);
    assert.match(first.value, /^# CTX /);
    assert.match(first.value, /## EDIT/);
    assert.match(first.value, /## PROOF/);
    cold += first.ms;
    warm += second.ms;
    console.log(`${item.name}: cold=${first.ms.toFixed(1)}ms warm=${second.ms.toFixed(1)}ms`);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
}

const repositoryCases = [
  { keywords: ["context_service", "callContextToolOrLocal"], task: "global context service routing" },
  { keywords: ["read_files", "edit_files"], task: "N-API filesystem tool routing" },
  {
    files: ["src-tauri/src/context_service.rs", "scripts/nova-context-client.mjs"],
    task: "inspect global context implementation",
  },
];
for (const params of repositoryCases) {
  const output = await callNapiTool("fast_context", process.cwd(), params);
  assert.equal(await callNapiTool("fast_context", process.cwd(), params), output, `repository deterministic: ${JSON.stringify(params)}`);
  assert.match(output, /^# CTX /);
  assert.match(output, /## EDIT/);
  assert.match(output, /## PROOF/);
}

const symbolsRoot = await fixture({
  "src/a.ts": "export function pick() {\n  return 1;\n}\n",
  "src/b.ts": "import { pick } from './a';\nexport const value = pick();\n",
});
try {
  assert.equal(
    await callNapiTool("find_symbols", symbolsRoot, { names: ["pick", "missingSymbol"] }),
    await findSymbols({ names: ["pick", "missingSymbol"] }, symbolsRoot),
  );
  assert.equal(
    await callNapiTool("code_map", symbolsRoot, { scope: "src/" }),
    await codeMap({ scope: "src/" }, symbolsRoot),
  );
} finally {
  await rm(symbolsRoot, { recursive: true, force: true });
}

console.log(`audit: ${cases.length} fixture + ${repositoryCases.length} repository deterministic cases + find_symbols/code_map parity passed`);
console.log(`aggregate: cold=${cold.toFixed(1)}ms warm=${warm.toFixed(1)}ms`);
