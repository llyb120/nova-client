// Regenerates the differential golden vectors for the Rust `pi_core` port from
// the authoritative node implementation (scripts/alkaid-core.mjs).
//
// Dev-time only: `cargo test` in src-tauri/pi_core needs no node. Run this
// whenever the ported node functions change, then re-run `cargo test`.
//
//   node scripts/pi-golden.mjs
//
// NOTE: mergeAlkaidUsage mutates its `total` argument in place, so inputs are
// deep-cloned before being recorded and before being passed to the function.
import * as core from "./alkaid-core.mjs";
import { applySmartEdits } from "./alkaid-smart-edit.mjs";
import { writeFileSync } from "node:fs";

const out = {};

const clampCases = [
  ["", 10], ["hello", 10], ["hello world", 10], ["abcdefghij", 10],
  ["中文中文中文", 10], ["a中b", 5], ["😀😀😀", 8], ["a😀b", 6],
  ["x".repeat(50), 20], ["é".repeat(30), 15], [null, 10], [undefined, 8], [12345, 3],
];
out.clampToolOutputText = clampCases.map(([text, maxChars]) => ({
  input: { text, maxChars }, expected: core.clampToolOutputText(text, maxChars),
}));

const govTexts = ["short", "y".repeat(100), "中文".repeat(200), "😀".repeat(100), "line\n".repeat(5000)];
const govCases = [];
for (const t of govTexts) {
  for (const maxBytes of [32, 64, 128, 1024]) {
    const result = { content: [{ type: "text", text: t }] };
    const governed = await core.governToolResult(result, { maxBytes, toolCallId: "call-1", toolName: "bash" });
    govCases.push({ input: { text: t, maxBytes }, expected: {
      text: governed.content[0].text, details: governed.details ?? null, nonText: governed.content.slice(1),
    }});
  }
}
out.governToolResult = govCases;

out.clampPromptCacheKey = [
  "", "  ", null, undefined, "abc", "  trimmed  ", "x".repeat(100),
  "中文".repeat(50), "😀".repeat(50), "a".repeat(64), "a".repeat(65),
].map((key) => ({ input: { key }, expected: core.clampPromptCacheKey(key) ?? null }));

const injectCases = [
  [{}, "sess-123"], [{ prompt_cache_key: "existing" }, "sess-123"], [{ promptCacheKey: "existing2" }, "sess-123"],
  [{ model: "gpt" }, ""], [{ model: "gpt" }, null], [{ model: "gpt" }, "x".repeat(100)], [null, "sess"], [[1,2], "sess"],
];
out.injectOpenAIPromptCacheKey = injectCases.map(([payload, sessionId]) => ({
  input: { payload: structuredClone(payload), sessionId },
  expected: core.injectOpenAIPromptCacheKey(payload, sessionId) ?? null,
}));

const payloadCases = [
  [{ input: [{ type: "function_call_output", output: "z".repeat(30) }] }, 20],
  [{ input: [{ type: "function_call_output", output: "中文".repeat(30) }] }, 20],
  [{ input: [{ type: "function_call_output", output: [{ type: "input_text", text: "w".repeat(40) }] }] }, 20],
  [{ input: [{ type: "message", output: "keep" }] }, 5],
  [{ messages: [{ role: "tool", content: "q".repeat(30) }, { role: "user", content: "keep" }] }, 20],
  [{ messages: [{ role: "tool", content: [{ type: "text", text: "arr" }] }] }, 5],
  [null, 10], [[], 10],
];
out.clampOpenAIPayloadToolOutputs = payloadCases.map(([payload, maxChars]) => ({
  input: { payload: structuredClone(payload), maxChars },
  expected: core.clampOpenAIPayloadToolOutputs(payload, maxChars) ?? null,
}));

const usageCases = [
  [null, null],
  [null, { input: 1, output: 2, cacheRead: 3, cacheWrite: 4 }],
  [{ input: 10, output: 20, cacheRead: 30, cacheWrite: 40 }, { input: 1, output: 2, cacheRead: 3, cacheWrite: 4 }],
  [{ input: 10, output: 20, cacheRead: 30, cacheWrite: 40 }, null],
  [{ input: 10, output: 20, cacheRead: 30, cacheWrite: 40 }, { input: "x", output: NaN, cacheRead: undefined }],
  [{ input: 5, output: 5, cacheRead: 0, cacheWrite: 0 }, { input: 1.5, output: 2.5 }],
];
out.mergeAlkaidUsage = usageCases.map(([total, usage]) => ({
  input: { total: structuredClone(total), usage: structuredClone(usage) },
  expected: core.mergeAlkaidUsage(structuredClone(total), structuredClone(usage)) ?? null,
}));

const enc = (s) => Buffer.from(s, "utf8");
const bufCases = [
  enc("hello"), enc("中文内容"),
  Buffer.from([0xff, 0xfe, 0x68, 0x00, 0x69, 0x00]),
  Buffer.from([0xfe, 0xff, 0x00, 0x68, 0x00, 0x69]),
  Buffer.from([0xef, 0xbb, 0xbf, 0x68, 0x69]),
  Buffer.from([0x68, 0x00, 0x69, 0x00, 0x6a, 0x00, 0x6b, 0x00, 0x6c, 0x00, 0x6d, 0x00, 0x6e, 0x00, 0x6f, 0x00]),
  Buffer.from([0x00, 0x68, 0x00, 0x69, 0x00, 0x6a, 0x00, 0x6b, 0x00, 0x6c, 0x00, 0x6d, 0x00, 0x6e, 0x00, 0x6f]),
  Buffer.from([]), Buffer.from([0x00]),
];
out.decodeTextBuffer = bufCases.map((b) => ({ input: { base64: b.toString("base64") }, expected: core.decodeTextBuffer(b) }));

const promptCases = [
  [{ cwd: "/home/u/proj", skills: [], readOnly: false, shellConfig: { shell: "/bin/bash", args: ["-c"], kind: "bash" }, systemPrompt: "" }],
  [{ cwd: "C:\\Users\\u\\proj", skills: [], readOnly: true, shellConfig: null, systemPrompt: "" }],
  [{ cwd: "/x", skills: [], readOnly: false, shellConfig: { shell: "powershell.exe", args: ["-c"], kind: "powershell" }, systemPrompt: "custom instr" }],
];
out.buildAlkaidSystemPrompt = promptCases.map(([options]) => ({ input: { options: structuredClone(options) }, expected: core.buildAlkaidSystemPrompt(options) }));

// --- applySmartEdits ---
const smartCases = [
  // exact single line
  { content: "alpha\nbeta\ngamma\n", edits: [["beta", "BETA"]], path: "a.txt" },
  // exact multi-line
  { content: "fn main() {\n    let x = 1;\n    println!(\"hi\");\n}\n", edits: [["    let x = 1;\n    println!(\"hi\");", "    let y = 2;"]], path: "b.rs" },
  // duplicate exact -> error
  { content: "foo\nbar\nfoo\n", edits: [["foo", "baz"]], path: "c.txt" },
  // empty oldText -> error
  { content: "abc\n", edits: [["", "x"]], path: "d.txt" },
  // rstrip: oldText has trailing spaces not in content
  { content: "line one\nkeep me\nline three\n", edits: [["keep me   ", "changed"]], path: "e.txt" },
  // unicode: smart quotes in oldText, ascii in content
  { content: "say \"hello\" now\n", edits: [["say “hello” now", "say “bye” now"]], path: "f.txt" },
  // relative-indent: same shape, deeper indentation in content
  { content: "outer\n        inner_a\n        inner_b\nend\n", edits: [["    inner_a\n    inner_b", "    inner_x\n    inner_y"]], path: "g.txt" },
  // fuzzy: one token differs
  { content: "def compute_total(items):\n    total = sum(items)\n    return total\n", edits: [["def compute_sum(items):\n    total = sum(items)\n    return total", "def compute_total(items):\n    total = sum(items)\n    return round(total)"]], path: "h.py" },
  // rebase indent: newText indented to oldText, matched region deeper
  { content: "begin\n        stmt_one\n        stmt_two\nfinish\n", edits: [["stmt_one\nstmt_two", "stmt_one\nstmt_extra\nstmt_two"]], path: "i.txt" },
  // overlap -> error
  { content: "aaaa bbbb cccc\n", edits: [["aaaa bbbb", "X"], ["bbbb cccc", "Y"]], path: "j.txt" },
  // no change -> error
  { content: "same\n", edits: [["same", "same"]], path: "k.txt" },
  // non-ASCII exact + edit
  { content: "第一行\n第二行\n第三行\n", edits: [["第二行", "第二行（改）"]], path: "l.txt" },
  // emoji exact
  { content: "start\n🎉 party 🎉\nend\n", edits: [["🎉 party 🎉", "🚀 launch 🚀"]], path: "m.txt" },
  // multiple non-overlapping edits
  { content: "one\ntwo\nthree\nfour\n", edits: [["one", "ONE"], ["three", "THREE"]], path: "n.txt" },
  // ambiguous rstrip -> error (two identical trimmed lines)
  { content: "dup  \nmid\ndup\n", edits: [["dup", "zap"]], path: "o.txt" },
  // CRLF normalization in edits
  { content: "p\nq\nr\n", edits: [["q\r\n", "Q\r\n"]], path: "p.txt" },
];
out.smartEdit = smartCases.map(({ content, edits, path }) => {
  try {
    const result = applySmartEdits(content, edits.map(([oldText, newText]) => ({ oldText, newText })), path);
    return { input: { content, edits, path }, expected: { ok: true, content: result.content, matches: result.matches } };
  } catch (error) {
    return { input: { content, edits, path }, expected: { ok: false, error: error instanceof Error ? error.message : String(error) } };
  }
});

// --- read_files (via the real tool) ---
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const utf16le = (s) => Buffer.from(s, "utf16le");
const readRoot = mkdtempSync(join(tmpdir(), "piread-"));
const readFilesTool = core.createFilesystemTools(readRoot, null).find((t) => t.name === "read_files");

const le = (s) => Buffer.from(s, "utf8");
const readCases = [
  { fileName: "ascii.txt", bytes: le("line1\nline2\nline3\n"), request: { path: "ascii.txt" } },
  { fileName: "ascii.txt", bytes: le("line1\nline2\nline3\n"), request: { path: "ascii.txt", offset: 2, limit: 1 } },
  { fileName: "ascii.txt", bytes: le("line1\nline2\nline3\n"), request: { path: "ascii.txt", offset: 10 } },
  { fileName: "multi.txt", bytes: le("l1\nl2\nl3\nl4\n"), request: { path: "multi.txt", limit: 2 } },
  { fileName: "empty.txt", bytes: le(""), request: { path: "empty.txt" } },
  { fileName: "crlf.txt", bytes: le("a\r\nb\r\nc"), request: { path: "crlf.txt" } },
  { fileName: "utf8bom.txt", bytes: Buffer.concat([Buffer.from([0xef, 0xbb, 0xbf]), le("héllo\nwörld\n")]), request: { path: "utf8bom.txt" } },
  { fileName: "utf16le.txt", bytes: Buffer.concat([Buffer.from([0xff, 0xfe]), utf16le("hi\nyo")]), request: { path: "utf16le.txt" } },
  { fileName: "utf16be.txt", bytes: Buffer.concat([Buffer.from([0xfe, 0xff]), Buffer.from("hi\nyo", "utf16le").swap16()]), request: { path: "utf16be.txt" } },
  { fileName: "bomless16.txt", bytes: utf16le("abcd\nefgh\nijkl"), request: { path: "bomless16.txt" } },
  { fileName: "chinese.txt", bytes: le("第一行\n第二行\n第三行\n"), request: { path: "chinese.txt", offset: 2 } },
  { fileName: "longline.txt", bytes: le("a".repeat(40000) + "\nsecond\n"), request: { path: "longline.txt" } },
  { fileName: "absent.txt", bytes: null, request: { path: "absent.txt" } },
];
const readGolden = [];
for (const { fileName, bytes, request } of readCases) {
  if (bytes) writeFileSync(join(readRoot, fileName), bytes);
  const res = await readFilesTool.execute("call", { paths: [request] });
  const element = JSON.parse(res.content[0].text)[0];
  const expected = "error" in element
    ? { ok: false }
    : { ok: true, result: element };
  readGolden.push({
    input: { fileName, fileBase64: bytes ? bytes.toString("base64") : null, request },
    expected,
  });
}
out.readFiles = readGolden;

writeFileSync("src-tauri/pi_core/testdata/golden.json", JSON.stringify(out));
console.log("wrote src-tauri/pi_core/testdata/golden.json", JSON.stringify(out).length, "bytes");
for (const k of Object.keys(out)) console.log(" ", k, out[k].length);
