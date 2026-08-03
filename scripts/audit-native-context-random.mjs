import { execFileSync } from "node:child_process";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { codeMap, contextBundle, findSymbols } from "./ctx-core.mjs";
import { callNapiTool } from "./nova-napi-tools.mjs";

const seed = Number.parseInt(process.env.NOVA_CONTEXT_FUZZ_SEED ?? "20250308", 10) >>> 0;
const caseCount = Math.max(1, Number.parseInt(process.env.NOVA_CONTEXT_FUZZ_CASES ?? "64", 10) || 64);
const caseStart = Math.max(0, Number.parseInt(process.env.NOVA_CONTEXT_FUZZ_START ?? "0", 10) || 0);
const keepFailures = process.env.NOVA_CONTEXT_FUZZ_KEEP === "1";

function mulberry32(initial) {
  let state = initial >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let value = state;
    value = Math.imul(value ^ (value >>> 15), value | 1);
    value ^= value + Math.imul(value ^ (value >>> 7), value | 61);
    return ((value ^ (value >>> 14)) >>> 0) / 0x100000000;
  };
}

function integer(random, min, max) {
  return min + Math.floor(random() * (max - min + 1));
}

function pick(random, values) {
  return values[integer(random, 0, values.length - 1)];
}

function sample(random, values, count) {
  const pool = [...values];
  const result = [];
  while (pool.length && result.length < count) {
    result.push(pool.splice(integer(random, 0, pool.length - 1), 1)[0]);
  }
  return result;
}

function caseRandom(index) {
  return mulberry32((seed ^ Math.imul(index + 1, 0x9e3779b1)) >>> 0);
}

function makeRepository(index) {
  const random = caseRandom(index);
  const moduleCount = integer(random, 5, 12);
  const featureCount = integer(random, 2, 5);
  const symbols = Array.from({ length: moduleCount }, (_, module) => `fuzz_${index}_${module}`);
  const moduleFiles = Array.from({ length: moduleCount }, (_, module) => (
    `src/feature${module % featureCount}/module${module}.${pick(random, ["ts", "ts", "ts", "js"])}`
  ));
  const files = {};

  for (let module = 0; module < moduleCount; module += 1) {
    const file = moduleFiles[module];
    const dependency = module > 0 ? integer(random, 0, module - 1) : -1;
    const dependencyBase = dependency >= 0 ? moduleFiles[dependency].replace(/\.(?:ts|js)$/, "") : "";
    const dependencyFile = dependency >= 0 ? `../${dependencyBase.slice("src/".length)}` : "";
    const imported = dependency >= 0
      ? `import { ${symbols[dependency]} } from '${dependencyFile}';\n`
      : "";
    const call = dependency >= 0 ? `${symbols[dependency]}(value)` : "value";
    const padding = Array.from(
      { length: integer(random, 0, 28) },
      (_, line) => `  const pad_${module}_${line} = ${line};`,
    ).join("\n");
    const nested = random() < 0.45
      ? `\n  function nested_${module}() {\n    return '${module % 2 ? "目标" : "target"}_${index}_${module}';\n  }`
      : "";
    files[file] = `${imported}export function ${symbols[module]}(value = ${module}) {${nested}\n${padding}\n  return ${call};\n}\n`;
  }

  files[`src/shared/constants${index}.ts`] = [
    `export const SHARED_${index} = '${"界".repeat(integer(random, 1, 24))}';`,
    `export const FLAG_${index} = ${index % 2 === 0};`,
    "",
  ].join("\n");
  files[`tests/consumer${index}.test.ts`] = [
    `import { ${symbols.at(-1)} } from '../${moduleFiles.at(-1).replace(/\.(?:ts|js)$/, "")}';`,
    `export const RESULT_${index} = ${symbols.at(-1)}();`,
    "",
  ].join("\n");
  files[`config/settings${index}.toml`] = `feature_${index} = true\nlabel = "随机-${index}"\n`;

  const keywordCount = integer(random, 1, Math.min(5, symbols.length));
  const keywords = sample(random, symbols, keywordCount);
  const params = pick(random, [
    { keywords },
    { keywords, task: `inspect ${keywords.join(" and ")} dependency behavior` },
    { task: `修改 ${keywords[0]} 的 target_${index} 逻辑` },
    { files: sample(random, Object.keys(files).filter((file) => file.endsWith(".ts") || file.endsWith(".js")), integer(random, 1, 3)), task: `inspect fuzz case ${index}` },
    { keywords, maxBytes: pick(random, [4096, 6144, 8192, 12288]) },
    { keywords, budget: pick(random, [120, 240, 480, 700]) },
  ]);

  return { files, params, symbols, mutationFile: Object.keys(files).find((file) => file.endsWith(".ts")) };
}

async function fixture(files) {
  const root = await mkdtemp(join(tmpdir(), "nova-native-context-fuzz-"));
  for (const [relative, content] of Object.entries(files)) {
    const absolute = join(root, relative);
    await mkdir(dirname(absolute), { recursive: true });
    await writeFile(absolute, content);
  }
  const git = (...args) => execFileSync("git", args, { cwd: root, stdio: "ignore", windowsHide: true });
  git("init", "-q");
  git("config", "user.email", "native-fuzz@nova.local");
  git("config", "user.name", "Nova Native Fuzz");
  git("add", "-A");
  git("commit", "-qm", "fixture");
  return root;
}

function byteExcerpt(buffer, offset) {
  const start = Math.max(0, offset - 80);
  const end = Math.min(buffer.length, offset + 160);
  return JSON.stringify(buffer.subarray(start, end).toString("utf8"));
}

function assertByteEqual(actual, expected, label) {
  const actualBytes = Buffer.from(actual, "utf8");
  const expectedBytes = Buffer.from(expected, "utf8");
  if (actualBytes.equals(expectedBytes)) return;
  const common = Math.min(actualBytes.length, expectedBytes.length);
  let offset = 0;
  while (offset < common && actualBytes[offset] === expectedBytes[offset]) offset += 1;
  throw new Error([
    `${label}: byte mismatch at offset ${offset}`,
    `JS bytes=${expectedBytes.length} excerpt=${byteExcerpt(expectedBytes, offset)}`,
    `Rust bytes=${actualBytes.length} excerpt=${byteExcerpt(actualBytes, offset)}`,
  ].join("\n"));
}

async function compare(tool, root, params, jsRun) {
  const js = await jsRun();
  const rust = await callNapiTool(tool, root, params);
  assertByteEqual(rust, js, tool);
}

let completed = 0;
for (let index = caseStart; index < caseStart + caseCount; index += 1) {
  const generated = makeRepository(index);
  const root = await fixture(generated.files);
  let failed = false;
  try {
    await compare("fast_context", root, generated.params, () => contextBundle(generated.params, root));
    await compare("fast_context", root, generated.params, () => contextBundle(generated.params, root));

    const names = [
      ...sample(caseRandom(index + 10_000), generated.symbols, Math.min(3, generated.symbols.length)),
      `missing_${index}`,
    ];
    await compare("find_symbols", root, { names }, () => findSymbols({ names }, root));

    const scope = index % 3 === 0 ? "src/" : index % 3 === 1 ? `src/feature${index % 4}/` : undefined;
    const codeMapParams = scope ? { scope } : {};
    await compare("code_map", root, codeMapParams, () => codeMap(codeMapParams, root));

    const mutationSymbol = `mutated_${index}`;
    const mutationPath = join(root, generated.mutationFile);
    await writeFile(
      mutationPath,
      `${generated.files[generated.mutationFile]}\nexport function ${mutationSymbol}() { return '${"变".repeat((index % 7) + 1)}'; }\n`,
    );
    const mutationParams = { keywords: [mutationSymbol], task: `inspect ${mutationSymbol}` };
    await compare("fast_context", root, mutationParams, () => contextBundle(mutationParams, root));
    await compare("find_symbols", root, { names: [mutationSymbol] }, () => findSymbols({ names: [mutationSymbol] }, root));
    completed += 1;
  } catch (error) {
    failed = true;
    console.error(`random differential failed: seed=${seed} case=${index} root=${root}`);
    console.error(`params=${JSON.stringify(generated.params)}`);
    throw error;
  } finally {
    if (!failed || !keepFailures) await rm(root, { recursive: true, force: true });
  }
}

console.log(`random byte-parity: ${completed}/${caseCount} cases passed (seed=${seed}, start=${caseStart})`);
