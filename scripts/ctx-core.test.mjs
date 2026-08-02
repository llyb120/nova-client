import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtemp, rm, writeFile, mkdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

const { scanSource, innermostUnit, getIndex } = await import("./ctx-index.mjs");
const { contextBundle, codeMap, findSymbols, searchText } = await import("./ctx-core.mjs");

/** 建一个一次性 git 仓库当夹具，避免依赖本仓库内容。 */
async function fixture(files) {
  const dir = await mkdtemp(join(tmpdir(), "ctx-core-test-"));
  for (const [rel, body] of Object.entries(files)) {
    const abs = join(dir, rel);
    await mkdir(join(abs, ".."), { recursive: true });
    await writeFile(abs, body);
  }
  const git = (...args) => execFileSync("git", args, { cwd: dir, stdio: "ignore" });
  git("init", "-q");
  git("config", "user.email", "t@t.t");
  git("config", "user.name", "t");
  git("add", "-A");
  git("commit", "-qm", "init");
  return dir;
}

test("scanSource: 花括号配对得到真实符号边界与嵌套层级", () => {
  const { total, syms } = scanSource(
    [
      "export function outer(a) {",
      "  const inner = (b) => {",
      "    return b + 1;",
      "  };",
      "  return inner(a);",
      "}",
      "",
      "export const NAME = 'x';",
    ].join("\n"),
    "a.ts",
  );
  assert.equal(total, 8);
  const outer = syms.find((s) => s.name === "outer");
  assert.deepEqual([outer.ln, outer.end, outer.depth, outer.exp], [1, 6, 0, true]);
  const inner = syms.find((s) => s.name === "inner");
  assert.deepEqual([inner.ln, inner.end, inner.depth], [2, 4, 1]);
  const name = syms.find((s) => s.name === "NAME");
  assert.deepEqual([name.ln, name.end], [8, 8]);
});

test("scanSource: 字符串/注释/正则/模板串里的花括号不影响边界", () => {
  const src = [
    "export function f() {",
    "  const re = /\\/tests?\\{\\//i;",
    "  const s = \"}{ not code\";",
    "  const t = `a${ { k: 1 } }b`;",
    "  // } stray",
    "  /* } stray */",
    "  return re.test(s) ? t : '';",
    "}",
    "export function g() { return 1; }",
  ].join("\n");
  const { syms } = scanSource(src, "a.ts");
  const f = syms.find((s) => s.name === "f");
  assert.deepEqual([f.ln, f.end], [1, 8]);
  assert.ok(syms.some((s) => s.name === "g" && s.ln === 9));
});

test("scanSource: Rust 生命周期/原始串/跨行串不破坏扫描", () => {
  const src = [
    "pub fn take<'a>(v: &'a str) -> String {",
    "    let raw = r#\"a\"{}b\"#;",
    "    let multi = \"line1",
    "line2 {\";",
    "    format!(\"{} {}\", raw, multi)",
    "}",
    "impl Foo for Bar {",
    "    pub fn m(&self) -> u8 { 1 }",
    "}",
  ].join("\n");
  const { syms } = scanSource(src, "a.rs");
  const take = syms.find((s) => s.name === "take");
  assert.deepEqual([take.ln, take.end, take.exp], [1, 6, true]);
  const im = syms.find((s) => s.kind === "impl");
  assert.deepEqual([im.ln, im.end], [7, 9]);
  const m = syms.find((s) => s.name === "m");
  assert.equal(m.depth, 1);
});

test("scanSource: Python 按缩进定界", () => {
  const { syms } = scanSource(
    [
      "class Runner:",
      "    def start(self):",
      "        return 1",
      "",
      "    def _stop(self):",
      "        return 2",
      "",
      "def top():",
      "    pass",
    ].join("\n"),
    "a.py",
  );
  const cls = syms.find((s) => s.name === "Runner");
  assert.deepEqual([cls.ln, cls.end, cls.kind, cls.depth], [1, 6, "class", 0]);
  const start = syms.find((s) => s.name === "start");
  assert.deepEqual([start.ln, start.end, start.kind, start.depth], [2, 3, "method", 1]);
  assert.equal(syms.find((s) => s.name === "_stop").exp, false);
  const top = syms.find((s) => s.name === "top");
  assert.deepEqual([top.ln, top.end], [8, 9]);
});

test("scanSource: 提取 JS import 与 Rust use 的具名导入", () => {
  const js = scanSource(
    [
      "import Def, { a, b as c, type T } from './x';",
      "import * as ns from '../y';",
      "import {",
      "  multi,",
      "  line,",
      "} from './multi';",
      "export { re } from './re';",
    ].join("\n"),
    "src/m/n.ts",
  );
  assert.deepEqual(js.imports, [
    { name: "a", from: "./x" },
    { name: "c", from: "./x" },
    { name: "T", from: "./x" },
    { name: "Def", from: "./x" },
    { name: "ns", from: "../y" },
    { name: "multi", from: "./multi" },
    { name: "line", from: "./multi" },
    { name: "re", from: "./re" },
  ]);
  const rs = scanSource(
    ["use crate::settings::{Settings, Store as S};", "use super::helper;", "fn f() {}"].join("\n"),
    "src-tauri/src/a/b.rs",
  );
  assert.deepEqual(rs.imports, [
    { name: "Settings", from: "crate::settings::Settings" },
    { name: "S", from: "crate::settings::Store" },
    { name: "helper", from: "super::helper" },
  ]);
});

test("innermostUnit: 命中落到最内层单元", () => {
  const { syms } = scanSource(
    ["export function outer() {", "  function inner() {", "    hit();", "  }", "}"].join("\n"),
    "a.ts",
  );
  assert.equal(innermostUnit(syms, 3).name, "inner");
  assert.equal(innermostUnit(syms, 1).name, "outer");
});

test("fast_context: 命中给完整符号体, 跨文件 import 依赖定义完整打包进 DEPS", async () => {
  // 填充到 FULL_FILE_MAX 以上，强制走"按符号单元"而不是整文件
  const filler = Array.from({ length: 160 }, (_, i) => `export const PAD_${i} = ${i};`).join("\n");
  const dir = await fixture({
    "src/target.ts": [
      "import { helperFn } from './helper';",
      "",
      "export function targetFn(input: string) {",
      "  const cleaned = input.trim();",
      "  return helperFn(cleaned);",
      "}",
      "",
      filler,
    ].join("\n"),
    "src/helper.ts": [
      "const LOCAL_ONLY = 1;",
      "",
      "export function helperFn(v: string) {",
      "  return `${v}!${LOCAL_ONLY}`;",
      "}",
    ].join("\n"),
  });
  try {
    const out = await contextBundle({ keywords: ["targetFn"] }, dir);
    // 命中单元整体给出（含函数尾部的 }），不是前缀截断；输出不存在 partial
    assert.match(out, /## EDIT/);
    assert.match(out, /### src\/target\.ts \(167L\) shown=3-6/);
    assert.match(out, /@@ 3-6 fn targetFn \[def\]\nexport function targetFn/);
    assert.match(out, /return helperFn\(cleaned\);\n\}/);
    assert.doesNotMatch(out, /partial/);
    // 依赖定义沿 import 精确解析，完整单元进 DEPS
    assert.match(out, /## DEPS/);
    assert.match(out, /### src\/helper\.ts/);
    assert.match(out, /@@ 3-5 fn helperFn \[dep\]\nexport function helperFn/);
    assert.match(out, /return `\$\{v\}!\$\{LOCAL_ONLY\}`;\n\}/);
    // 别处文件里的非导出局部量不会被当成依赖
    assert.doesNotMatch(out, /LOCAL_ONLY = 1/);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("fast_context: Rust use 路径的依赖同样精确闭包", async () => {
  const filler = Array.from({ length: 160 }, (_, i) => `pub const PAD_${i}: u8 = ${i % 256};`).join("\n");
  const dir = await fixture({
    "src/settings.rs": [
      "pub struct Settings {",
      "    pub enabled: bool,",
      "    pub level: u8,",
      "}",
    ].join("\n"),
    "src/main.rs": [
      "use crate::settings::Settings;",
      "",
      "pub fn loadSettings() {",
      "    let _s = Settings { enabled: true, level: 1 };",
      "}",
      "",
      filler,
    ].join("\n"),
  });
  try {
    const out = await contextBundle({ keywords: ["loadSettings"] }, dir);
    assert.match(out, /@@ 3-5 fn loadSettings \[def\]/);
    assert.match(out, /## DEPS/);
    assert.match(out, /### src\/settings\.rs/);
    assert.match(out, /pub struct Settings \{/);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("fast_context: 大符号体默认整给, 无 partial; 预算放不下时降级 SIG 不倒代码", async () => {
  const body = Array.from({ length: 200 }, (_, i) => `  const v${i} = ${i};`);
  body[150] = "  const marked = MARKER_TOKEN;";
  const dir = await fixture({
    "src/big.ts": ["export function big() {", ...body, "  return 0;", "}"].join("\n"),
  });
  try {
    const full = await contextBundle({ keywords: ["MARKER_TOKEN"] }, dir);
    // 默认预算下整个大函数完整给出
    assert.match(full, /export function big\(\) \{/);
    assert.match(full, /const marked = MARKER_TOKEN;/);
    assert.match(full, /const v10 = 10;/);
    assert.doesNotMatch(full, /partial/);

    const tight = await contextBundle({ keywords: ["MARKER_TOKEN"], budget: 100 }, dir);
    // 预算放不下 → 定义进 SIG，正文不倾倒
    assert.match(tight, /## SIG/);
    assert.match(tight, /src\/big\.ts:1 export function big\(\) \{/);
    assert.ok(!tight.includes("const v10 = 10;"), "预算放不下时不应出现函数体");
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("fast_context: 修改计划会从目标体反向检索错误处理方", async () => {
  const filler = Array.from({ length: 130 }, (_, i) => `export const PAD_${i} = ${i};`).join("\n");
  const dir = await fixture({
    "src/auth/refreshSession.ts": [
      "import { SessionExpiredError } from './errors';",
      "export function refreshSession(token) {",
      "  if (!token) throw new SessionExpiredError('expired');",
      "  return token;",
      "}",
      filler,
    ].join("\n"),
    "src/auth/errors.ts": "export class SessionExpiredError extends Error {}\n",
    "src/ui/sessionBoundary.ts": [
      "import { SessionExpiredError } from '../auth/errors';",
      "export function handleSessionFailure(error) {",
      "  if (error instanceof SessionExpiredError) return 'login';",
      "  throw error;",
      "}",
    ].join("\n"),
  });
  try {
    const plain = await contextBundle({ keywords: ["refreshSession"] }, dir);
    assert.doesNotMatch(plain, /export function handleSessionFailure/);
    const planned = await contextBundle({
      keywords: ["refreshSession"],
      task: "修改 refreshSession 的失败和错误处理，保持会话过期行为",
    }, dir);
    assert.match(planned, /export function handleSessionFailure/);
    assert.match(planned, /SessionExpiredError/);
    assert.match(planned, /## PROOF \(任务闭包检查\)/);
    assert.match(planned, /错误处理: 已闭合/);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("fast_context: 任务闭包会展开二层导入依赖并输出缺口证明", async () => {
  const filler = Array.from({ length: 120 }, (_, i) => `export const PAD_${i} = ${i};`).join("\n");
  const dir = await fixture({
    "src/target.ts": [
      "import { buildEnvelope } from './helper';",
      "export function targetFlow(value) { return buildEnvelope(value); }",
      filler,
    ].join("\n"),
    "src/helper.ts": [
      "import { ResultEnvelope } from './types';",
      "export function buildEnvelope(value) { return new ResultEnvelope(value); }",
    ].join("\n"),
    "src/types.ts": [
      "export class ResultEnvelope {",
      "  constructor(value) { this.value = value; }",
      "}",
    ].join("\n"),
  });
  try {
    const out = await contextBundle({ keywords: ["targetFlow"] }, dir);
    assert.match(out, /buildEnvelope/);
    assert.match(out, /ResultEnvelope/);
    assert.match(out, /\[dep2\]/);
    assert.match(out, /## PROOF \(任务闭包检查\)/);
    assert.match(out, /目标定义: 已闭合/);
    assert.match(out, /依赖定义: 已闭合/);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("fast_context: task 描述提词可独立检索命中", async () => {
  const dir = await fixture({
    "src/pipe.ts": [
      "export function renderPipeline(node) {",
      "  return node ? String(node) : '';",
      "}",
    ].join("\n"),
  });
  try {
    const out = await contextBundle({ task: "调整 renderPipeline 的渲染逻辑" }, dir);
    assert.match(out, /export function renderPipeline/);
    assert.match(out, /return node \? String\(node\) : '';/);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("fast_context: 中文 task 可独立提词检索命中", async () => {
  const dir = await fixture({
    "src/settings.ts": "export const 设置面板 = { label: '快速上下文开关' };\n",
  });
  try {
    const out = await contextBundle({ task: "给设置面板增加一个快速上下文开关" }, dir);
    assert.match(out, /export const 设置面板/);
    assert.match(out, /快速上下文开关/);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("fast_context: 未展开引用进 IMPACT 清单", async () => {
  const many = {};
  for (let i = 0; i < 12; i++) {
    many[`src/f${i}.ts`] = `export function f${i}() {\n  return sharedFlag;\n}\n`;
  }
  many["src/flag.ts"] = "export const sharedFlag = true;\n";
  const dir = await fixture(many);
  try {
    const out = await contextBundle({ keywords: ["sharedFlag"] }, dir);
    assert.match(out, /## IMPACT/);
    assert.match(out, /src\/f\d+\.ts:2\s+return sharedFlag;/);
    assert.match(out, /其它命中文件\(未展开\)/);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("fast_context: 小文件整给, files 参数点名的文件优先整给", async () => {
  const dir = await fixture({
    "src/small.ts": ["export const A = 1;", "export const B = 2;"].join("\n"),
    "src/other.ts": "export const C = 3;\n",
  });
  try {
    const byKeyword = await contextBundle({ keywords: ["export const A"] }, dir);
    assert.match(byKeyword, /### src\/small\.ts \(2L\) FULL/);
    const byFiles = await contextBundle({ files: ["src/other.ts"] }, dir);
    assert.match(byFiles, /### src\/other\.ts \(1L\) FULL/);
    assert.match(byFiles, /export const C = 3;/);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("fast_context: 文件名命中的主题文件优先打包, 正文零命中也给", async () => {
  const dir = await fixture({
    "src/payroll_engine.ts": [
      "export function calculateWage(hours: number, rate: number) {",
      "  const base = hours * rate;",
      "  return Math.round(base * 100) / 100;",
      "}",
      "",
      "export function formatWage(v: number) {",
      "  return `$${v.toFixed(2)}`;",
      "}",
    ].join("\n"),
    "src/unrelated.ts": [
      "export const LABEL = 'payroll';",
      "export function label() {",
      "  return LABEL;",
      "}",
    ].join("\n"),
  });
  try {
    const out = await contextBundle({ keywords: ["payroll"] }, dir);
    // 主题文件正文零命中，仍被整给
    assert.match(out, /### src\/payroll_engine\.ts \(8L\) FULL/);
    assert.match(out, /export function calculateWage/);
    assert.ok(out.indexOf("### src/payroll_engine.ts") < out.indexOf("### src/unrelated.ts"));
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("fast_context: 大主题文件按命中优先通读打包, 测试文件不抢预算", async () => {
  const filler = Array.from({ length: 830 }, (_, i) => `export const PAD_${i} = ${i};`).join("\n");
  const dir = await fixture({
    "src/zebra_engine.ts": [
      "export function zebraMarker() {",
      "  return 'zebra';",
      "}",
      "",
      filler,
      "",
      "export function laterFlow() {",
      "  return 42;",
      "}",
    ].join("\n"),
    "src/zebra.test.ts": [
      "import { zebraMarker } from './zebra_engine';",
      "export const sees = zebraMarker();",
    ].join("\n"),
  });
  try {
    const out = await contextBundle({ keywords: ["zebra"] }, dir);
    // 大主题（>800L）不整给，但命中单元与无命中单元都打包，且无 partial
    assert.match(out, /### src\/zebra_engine\.ts \(838L\) shown=/);
    assert.match(out, /export function zebraMarker/);
    assert.match(out, /export function laterFlow/);
    assert.doesNotMatch(out, /partial/);
    // 主题文件排在测试文件之前
    assert.ok(out.indexOf("### src/zebra_engine.ts") < out.indexOf("### src/zebra.test.ts"));
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("fast_context: 关键词含正则元字符不报错; 无命中给出可执行提示", async () => {
  const dir = await fixture({ "src/a.ts": "export const A = 1;\n" });
  try {
    const out = await contextBundle({ keywords: ["a[b", "/x)/"] }, dir);
    assert.match(out, /无命中/);
    const empty = await contextBundle({}, dir);
    assert.match(empty, /错误/);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("fast_context: 磁盘内容比索引新时按磁盘重扫, 行段不错位", async () => {
  const dir = await fixture({ "src/a.ts": "export function keep() {\n  return 1;\n}\n" });
  try {
    await getIndex(dir);
    await writeFile(join(dir, "src/a.ts"), "// inserted\n// inserted\nexport function keep() {\n  return 1;\n}\n");
    const out = await contextBundle({ keywords: ["keep"] }, dir);
    assert.match(out, /export function keep\(\) \{/);
    assert.doesNotMatch(out, /@@ 1-3/);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("code_map: 默认给文件清单, scope 给符号大纲", async () => {
  const dir = await fixture({
    "src/a.ts": "export function a() {\n  return 1;\n}\n",
    "lib/b.ts": "export function b() {\n  return 2;\n}\n",
  });
  try {
    const all = await codeMap({}, dir);
    assert.match(all, /src\/a\.ts/);
    assert.match(all, /lib\/b\.ts/);
    const scoped = await codeMap({ scope: "src/" }, dir);
    assert.match(scoped, /## src\/a\.ts \(3L\)/);
    assert.doesNotMatch(scoped, /lib\/b\.ts/);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("searchText: fast_context 与 find_symbols 共用批量搜索抽象", async () => {
  const dir = await fixture({
    "src/a.ts": "export function sharedSearch() { return 1; }\n",
    "src/b.ts": "export const use = sharedSearch();\n",
  });
  try {
    const rows = await searchText(dir, ["sharedSearch"], { word: true });
    assert.equal(rows.length, 2);
    assert.deepEqual([...new Set(rows.map((r) => r.file))].sort(), ["src/a.ts", "src/b.ts"]);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("find_symbols: 区分定义与引用", async () => {
  const dir = await fixture({
    "src/a.ts": "export function pick() {\n  return 1;\n}\n",
    "src/b.ts": "import { pick } from './a';\nexport const v = pick();\n",
  });
  try {
    const out = await findSymbols({ names: ["pick", "missingSymbol"] }, dir);
    assert.match(out, /## pick\s+defs=1 refs=3/);
    assert.match(out, /DEF src\/a\.ts:1-3/);
    assert.match(out, /src\/b\.ts:2/);
    assert.match(out, /## missingSymbol\s+defs=0 refs=0/);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("fast_context: 搜索未跟踪代码并把命中文件优先纳入索引", async () => {
  const dir = await fixture({ "src/base.ts": "export const base = 1;\n" });
  try {
    await writeFile(join(dir, "src/untracked.ts"), "export function freshUntracked() {\n  return 42;\n}\n");
    const out = await contextBundle({ keywords: ["freshUntracked"] }, dir);
    assert.match(out, /### src\/untracked\.ts/);
    assert.match(out, /export function freshUntracked/);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("focused index: 冷启动只扫描命中文件和直接依赖，不全扫仓库", async () => {
  const files = {
    "src/target.ts": "import { helper } from './helper';\nexport function target() { return helper(); }\n",
    "src/helper.ts": "export function helper() { return 1; }\n",
  };
  for (let i = 0; i < 80; i++) files[`src/noise-${i}.ts`] = `export const noise_${i} = ${i};\n`;
  const dir = await fixture(files);
  try {
    const index = await getIndex(dir, {
      priorityFiles: ["src/target.ts"],
      mode: "focused",
      dependencyDepth: 1,
    });
    assert.equal(index.stats.total, 82);
    assert.equal(index.stats.scanned, 2);
    assert(index.files["src/target.ts"]);
    assert(index.files["src/helper.ts"]);
    assert.equal(index.files["src/noise-0.ts"], undefined);
    assert.equal(index.complete, false);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("fast_context: maxBytes 只在完整边界收敛且不产生半截标记", async () => {
  const files = {};
  for (let i = 0; i < 8; i++) {
    const body = Array.from({ length: 35 }, (_, n) => `  const value_${i}_${n} = ${n};`);
    files[`src/f${i}.ts`] = [`export function bounded${i}() {`, ...body, "  return 1;", "}"].join("\n");
  }
  const dir = await fixture(files);
  try {
    const out = await contextBundle({ keywords: ["bounded"], maxBytes: 8192 }, dir);
    assert.ok(Buffer.byteLength(out, "utf8") <= 8192);
    assert.doesNotMatch(out, /\[truncated\]|partial/);
    assert.match(out, /## SIG/);
    assert.match(out, /src\/f\d+\.ts:1 export function bounded\d+\(\)/);
    for (const block of out.split(/^@@ /m).slice(1)) {
      assert.match(block, /\n\}/, "每个输出函数单元必须包含闭合花括号");
    }
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});
