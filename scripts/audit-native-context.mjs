import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { performance } from "node:perf_hooks";
import { codeMap, contextBundle, findSymbols } from "./ctx-core.mjs";
import { callNapiTool } from "./nova-napi-tools.mjs";

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
    name: "impact ordering",
    files: Object.fromEntries([
      ...Array.from({ length: 12 }, (_, index) => [`src/f${index}.ts`, `export function f${index}() {\n  return sharedFlag;\n}\n`]),
      ["src/flag.ts", "export const sharedFlag = true;\n"],
    ]),
    params: { keywords: ["sharedFlag"] },
  },
  {
    name: "task and nested unit",
    files: {
      "src/pipeline.ts": [
        "export function renderPipeline() {",
        "  function inner() {",
        "    return TARGET_INNER;",
        "  }",
        ...Array.from({ length: 120 }, (_, index) => `  const pad${index} = ${index};`),
        "  return inner();",
        "}",
      ].join("\n"),
    },
    params: { task: "with then 调整 renderPipeline TARGET_INNER 的渲染逻辑" },
  },
  {
    name: "Unicode and non-code",
    files: {
      "settings.toml": "feature_key = \"enabled\"\nother = 1\n",
      "src/中文.ts": `export function unicodeTarget() {\n  const value = '${"你".repeat(260)}';\n  return value;\n}`,
    },
    params: { keywords: ["unicodeTarget", "feature_key"] },
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

let jsCold = 0;
let rustCold = 0;
let jsWarm = 0;
let rustWarm = 0;
for (const item of cases) {
  const root = await fixture(item.files);
  try {
    const js1 = await timed(() => contextBundle(item.params, root));
    const rust1 = await timed(() => callNapiTool("fast_context", root, item.params));
    assert.equal(rust1.value, js1.value, `${item.name}: cold output`);
    const js2 = await timed(() => contextBundle(item.params, root));
    const rust2 = await timed(() => callNapiTool("fast_context", root, item.params));
    assert.equal(rust2.value, js2.value, `${item.name}: warm output`);
    jsCold += js1.ms;
    rustCold += rust1.ms;
    jsWarm += js2.ms;
    rustWarm += rust2.ms;
    console.log(`${item.name}: cold js=${js1.ms.toFixed(1)}ms rust=${rust1.ms.toFixed(1)}ms; warm js=${js2.ms.toFixed(1)}ms rust=${rust2.ms.toFixed(1)}ms`);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
}

const repositoryParityCases = [
  { keywords: ["context_service", "callContextToolOrLocal"], task: "global context service routing" },
  { keywords: ["read_files", "edit_files"], task: "N-API filesystem tool routing" },
];
for (const params of repositoryParityCases) {
  const js = await contextBundle(params, process.cwd());
  const rust = await callNapiTool("fast_context", process.cwd(), params);
  assert.equal(rust, js, `repository multi-target parity: ${JSON.stringify(params)}`);
  assert.equal(await callNapiTool("fast_context", process.cwd(), params), rust, `repository deterministic: ${JSON.stringify(params)}`);
}

// Explicit files plus task-only retrieval can legitimately consume independent cache state because it
// has no explicit symbol anchors. Keep deterministic closure coverage without weakening multi-target parity.
const repositoryDeterministicCases = [{
  files: ["src-tauri/src/context_service.rs", "scripts/nova-context-client.mjs"],
  task: "inspect global context implementation",
}];
for (const params of repositoryDeterministicCases) {
  const js = await contextBundle(params, process.cwd());
  const rust = await callNapiTool("fast_context", process.cwd(), params);
  assert.equal(await contextBundle(params, process.cwd()), js, `repository JS deterministic: ${JSON.stringify(params)}`);
  assert.equal(await callNapiTool("fast_context", process.cwd(), params), rust, `repository native deterministic: ${JSON.stringify(params)}`);
  for (const output of [js, rust]) {
    assert.match(output, /^# CTX /);
    assert.match(output, /## EDIT/);
    assert.match(output, /## PROOF/);
  }
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

console.log(`parity: ${cases.length} fixture + ${repositoryParityCases.length} repository multi-target + ${repositoryDeterministicCases.length} deterministic file cases + find_symbols + code_map passed`);
console.log(`aggregate cold: js=${jsCold.toFixed(1)}ms rust=${rustCold.toFixed(1)}ms speedup=${(jsCold / rustCold).toFixed(2)}x`);
console.log(`aggregate warm: js=${jsWarm.toFixed(1)}ms rust=${rustWarm.toFixed(1)}ms speedup=${(jsWarm / rustWarm).toFixed(2)}x`);
