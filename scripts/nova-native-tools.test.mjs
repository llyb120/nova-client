import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
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
