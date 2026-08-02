import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { codeMap, contextBundle, findSymbols } from "./ctx-core.mjs";
import { callNativeTool, closeNativeToolsClient, decodeMessagePack, encodeMessagePack, mayFallbackFromNative } from "./nova-native-tools.mjs";

test("MessagePack codec round-trips source-heavy RPC values without JSON escaping", () => {
  const value = {
    id: 7,
    method: "edit_files",
    root: "D:\\代码\\repo",
    params: {
      files: [{
        path: "src/a.ts",
        edits: [{
          oldText: "const re = /\\{.*\\}/;\nconst text = \"你好\";",
          newText: "const re = /\\[(.*)\\]/;\nconst text = `hello ${name}`;",
        }],
      }],
    },
  };
  assert.deepEqual(decodeMessagePack(encodeMessagePack(value)), value);
  const encoded = encodeMessagePack(value);
  assert(encoded.includes(Buffer.from("const re = /\\{.*\\}/;\n", "utf8")), "source bytes should remain literal inside MessagePack strings");
});

test("MessagePack codec supports frame-sized arrays and integer variants", () => {
  const value = { small: 127, medium: 65_535, negative: -32_000, float: 1.25, list: Array.from({ length: 20 }, (_, i) => i) };
  assert.deepEqual(decodeMessagePack(encodeMessagePack(value)), value);
});

test("edit fallback is allowed only when execution is known not to have happened", () => {
  const previous = process.env.NOVA_TOOLS_BACKEND;
  delete process.env.NOVA_TOOLS_BACKEND;
  try {
    assert.equal(mayFallbackFromNative("read_files", new Error("down")), true);
    assert.equal(mayFallbackFromNative("edit_files", Object.assign(new Error("down"), { definitelyNotExecuted: true })), true);
    assert.equal(mayFallbackFromNative("edit_files", new Error("unknown state")), false);
  } finally {
    if (previous === undefined) delete process.env.NOVA_TOOLS_BACKEND;
    else process.env.NOVA_TOOLS_BACKEND = previous;
  }
});

test("native sidecar serves read/edit/context over framed MessagePack", { skip: !process.env.NOVA_TOOLS_NATIVE_EXE }, async () => {
  const root = await mkdtemp(join(tmpdir(), "nova-native-tools-"));
  try {
    await writeFile(join(root, "a.ts"), "export function target() {\n  return 1;\n}\n");
    const read = await callNativeTool("read_files", root, { paths: [{ path: "a.ts", limit: 2 }] });
    assert.match(read[0].content, /export function target/);
    await callNativeTool("edit_files", root, { files: [{ path: "a.ts", edits: [{ oldText: "return 1", newText: "return 2" }] }] });
    assert.match(await readFile(join(root, "a.ts"), "utf8"), /return 2/);
    const context = await callNativeTool("fast_context", root, { keywords: ["target"] });
    assert.match(context, /## EDIT/);
    assert.match(context, /export function target/);
  } finally {
    closeNativeToolsClient();
    await rm(root, { recursive: true, force: true });
  }
});

test("native context tools are byte-for-byte compatible with the Node backend", { skip: !process.env.NOVA_TOOLS_NATIVE_EXE }, async () => {
  const fixture = async (files) => {
    const root = await mkdtemp(join(tmpdir(), "nova-context-parity-"));
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
  };
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
      name: "impact ordering and unexpanded files",
      files: Object.fromEntries([
        ...Array.from({ length: 12 }, (_, index) => [`src/f${index}.ts`, `export function f${index}() {\n  return sharedFlag;\n}\n`]),
        ["src/flag.ts", "export const sharedFlag = true;\n"],
      ]),
      params: { keywords: ["sharedFlag"] },
    },
    {
      name: "task tokens, ignored stop words and nested unit selection",
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
      name: "non-code search hit and Unicode clipping",
      files: {
        "settings.toml": "feature_key = \"enabled\"\nother = 1\n",
        "src/中文.ts": `export function unicodeTarget() {\n  const value = '${"你".repeat(260)}';\n  return value;\n}`,
      },
      params: { keywords: ["unicodeTarget", "feature_key"] },
    },
    {
      name: "hard budget whole-block fallback",
      files: Object.fromEntries(Array.from({ length: 8 }, (_, file) => [
        `src/f${file}.ts`,
        `export function bounded${file}() {\n${Array.from({ length: 35 }, (_, line) => `  const value_${file}_${line} = ${line};`).join("\n")}\n  return 1;\n}`,
      ])),
      params: { keywords: ["bounded"], maxBytes: 8192 },
    },
  ];
  try {
    for (const item of cases) {
      const root = await fixture(item.files);
      try {
        const node = await contextBundle(item.params, root);
        const native = await callNativeTool("fast_context", root, item.params);
        assert.equal(native, node, item.name);
      } finally {
        await rm(root, { recursive: true, force: true });
      }
    }
    const root = await fixture({
      "src/a.ts": "export function pick() {\n  return 1;\n}\n",
      "src/b.ts": "import { pick } from './a';\nexport const value = pick();\n",
    });
    try {
      assert.equal(
        await callNativeTool("find_symbols", root, { names: ["pick", "missingSymbol"] }),
        await findSymbols({ names: ["pick", "missingSymbol"] }, root),
      );
      assert.equal(
        await callNativeTool("code_map", root, { scope: "src/" }),
        await codeMap({ scope: "src/" }, root),
      );
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  } finally {
    closeNativeToolsClient();
  }
});
