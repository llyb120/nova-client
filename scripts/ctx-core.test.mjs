import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtemp, rm, writeFile, mkdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

const { scanSource, innermostUnit, getIndex } = await import("./ctx-index.mjs");
const { codeMap, searchText } = await import("./ctx-core.mjs");

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

test("searchText: polaris 使用批量搜索抽象", async () => {
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

test("searchText: files 可把后续图扩张限定到候选文件", async () => {
  const dir = await fixture({
    "src/a.ts": "export const bridge = 'shared';\n",
    "src/b.ts": "export const bridge = 'shared';\n",
  });
  try {
    const rows = await searchText(dir, ["shared"], { files: ["src/b.ts"] });
    assert.deepEqual([...new Set(rows.map((row) => row.file))], ["src/b.ts"]);
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

