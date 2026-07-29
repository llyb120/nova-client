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

writeFileSync("src-tauri/pi_core/testdata/golden.json", JSON.stringify(out));
console.log("wrote src-tauri/pi_core/testdata/golden.json", JSON.stringify(out).length, "bytes");
for (const k of Object.keys(out)) console.log(" ", k, out[k].length);
