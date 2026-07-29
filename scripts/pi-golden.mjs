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
import { formatSize, truncateHead, truncateTail, truncateLine } from "../node_modules/@earendil-works/pi-coding-agent/dist/core/tools/truncate.js";
import { resolvePath, normalizePath } from "../node_modules/@earendil-works/pi-coding-agent/dist/utils/paths.js";
import { createWriteTool } from "../node_modules/@earendil-works/pi-coding-agent/dist/core/tools/write.js";
import { createLsTool } from "../node_modules/@earendil-works/pi-coding-agent/dist/core/tools/ls.js";
import { runAgentLoop } from "../node_modules/@earendil-works/pi-agent-core/dist/agent-loop.js";
import { Agent } from "../node_modules/@earendil-works/pi-agent-core/dist/agent.js";
import { startedToolItem } from "./alkaid-bridge-common.mjs";
import { applyEditsToNormalizedContent } from "../node_modules/@earendil-works/pi-coding-agent/dist/core/tools/edit-diff.js";
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

// --- truncate utilities ---
out.formatSize = [
  0, 1, 512, 1023, 1024, 1280, 1536, 1792, 2048, 1587, 1588,
  51200, 1023 * 1024, 1048575, 1048576, 1280 * 1024, 1536 * 1024,
  5 * 1024 * 1024, 10 * 1024 * 1024 + 512 * 1024,
].map((bytes) => ({ input: { bytes }, expected: formatSize(bytes) }));

const headCases = [
  ["a\nb\nc", {}],
  ["l1\nl2\nl3\nl4\nl5", { maxLines: 2 }],
  ["aaaa\nbbbb\ncccc", { maxBytes: 10 }],
  ["x".repeat(100), { maxBytes: 10 }],
  ["", {}],
  ["single", {}],
  ["trailing\n", {}],
  ["中文行\n第二行\n第三行", { maxBytes: 12 }],
  ["line1\nline2\nline3\nline4", { maxLines: 2, maxBytes: 100 }],
  ["a".repeat(60000), {}],
];
out.truncateHead = headCases.map(([content, options]) => ({
  input: { content, options },
  expected: truncateHead(content, options),
}));

const tailCases = [
  ["l1\nl2\nl3\nl4\nl5", { maxLines: 2 }],
  ["short\n" + "y".repeat(100), { maxBytes: 20 }],
  ["a\nb\nc", {}],
  ["", {}],
  ["中文\n" + "字".repeat(100), { maxBytes: 30 }],
  ["e1\ne2\ne3\ne4\ne5\ne6", { maxLines: 3 }],
];
out.truncateTail = tailCases.map(([content, options]) => ({
  input: { content, options },
  expected: truncateTail(content, options),
}));

out.truncateLine = [
  ["short", 10],
  ["z".repeat(20), 10],
  ["exactly10c", 10],
  ["中文".repeat(10), 6],
  ["a😀b😀c😀d", 4],
  ["", 5],
].map(([line, maxChars]) => ({ input: { line, maxChars }, expected: truncateLine(line, maxChars) }));

// --- path resolution ---
const cwdOpt = { normalizeUnicodeSpaces: true, stripAtPrefix: true };
const resolveCases = [
  ["foo", "/base"],
  ["../foo", "/base/sub"],
  ["/abs/path", "/base"],
  ["./foo", "/base"],
  ["foo/../bar", "/base"],
  ["@foo", "/base"],
  ["foo\u00A0bar", "/base"],
  ["a//b", "/base"],
  ["", "/base"],
  [".", "/base"],
  ["..", "/base/sub"],
  ["../..", "/base/sub/deep"],
  ["/", "/base"],
  ["foo/", "/base"],
  ["./", "/base"],
  ["foo/./bar", "/base"],
  ["\u2003padded\u3000", "/base"],
  ["@/abs/x", "/base"],
  ["sub/dir/file.txt", "/home/u/proj"],
];
out.resolveToCwd = resolveCases.map(([input, base]) => ({
  input: { input, base },
  expected: resolvePath(input, base, cwdOpt),
}));

const normalizeCases = [
  ["~", { homeDir: "/HOME" }],
  ["~/docs", { homeDir: "/HOME" }],
  ["~/a/../b", { homeDir: "/HOME" }],
  ["@~/x", { normalizeUnicodeSpaces: true, stripAtPrefix: true, expandTilde: false }],
  ["plain", {}],
  ["  spaced  ", { trim: true }],
  ["  spaced  ", {}],
  ["a\u00A0b\u3000c", { normalizeUnicodeSpaces: true }],
  ["@mention", { stripAtPrefix: true }],
  ["@mention", {}],
];
out.normalizePath = normalizeCases.map(([input, options]) => ({
  input: { input, options },
  expected: normalizePath(input, options),
}));

// --- write tool ---
import { mkdirSync } from "node:fs";
const writeRoot = mkdtempSync(join(tmpdir(), "piwrite-"));
const writeTool = createWriteTool(writeRoot, {});
const writeCases = [
  { path: "new/nested/file.txt", content: "hello" },
  { path: "cjk.txt", content: "中文😀" },
  { path: "empty.txt", content: "" },
  { path: "overwrite.txt", content: "first" },
];
const writeGolden = [];
for (const { path, content } of writeCases) {
  const res = await writeTool.execute("call", { path, content });
  writeGolden.push({ input: { path, content }, expected: { ok: true, text: res.content[0].text } });
}
// Overwrite again to confirm last-write-wins message shape.
const over = await writeTool.execute("call", { path: "overwrite.txt", content: "second longer" });
writeGolden.push({ input: { path: "overwrite.txt", content: "second longer" }, expected: { ok: true, text: over.content[0].text } });
out.writeTool = writeGolden;

// --- ls tool ---
const lsRoot = mkdtempSync(join(tmpdir(), "pils-"));
for (const name of ["apple", "Banana", "cherry", "file10", "file2"]) writeFileSync(join(lsRoot, name), "x");
mkdirSync(join(lsRoot, "Zulu"));
mkdirSync(join(lsRoot, "alpha"));
const emptyDir = join(lsRoot, "emptydir");
mkdirSync(emptyDir);
const manyDir = join(lsRoot, "many");
mkdirSync(manyDir);
for (let i = 1; i <= 5; i++) writeFileSync(join(manyDir, `e${i}`), "x");
writeFileSync(join(lsRoot, "afile"), "x");

const lsTool = createLsTool(lsRoot, {});
const lsGolden = [];
const lsRun = async (input) => {
  try {
    const res = await lsTool.execute("call", input);
    return { ok: true, text: res.content[0].text, details: res.details ?? null };
  } catch (error) {
    return { ok: false, error: error instanceof Error ? error.message : String(error) };
  }
};
lsGolden.push({ input: { cwd: lsRoot, args: { path: "." } }, expected: await lsRun({ path: "." }) });
lsGolden.push({ input: { cwd: lsRoot, args: { path: "emptydir" } }, expected: await lsRun({ path: "emptydir" }) });
lsGolden.push({ input: { cwd: lsRoot, args: { path: "many", limit: 3 } }, expected: await lsRun({ path: "many", limit: 3 }) });
lsGolden.push({ input: { cwd: lsRoot, args: { path: "many" } }, expected: await lsRun({ path: "many" }) });
// Deterministic error: absolute missing path (message is tool-constructed, no temp path).
lsGolden.push({ input: { cwd: lsRoot, args: { path: "/pi-fixed-cwd/missing" } }, expected: await lsRun({ path: "/pi-fixed-cwd/missing" }) });
// Not-a-directory: real file; compare presence only (message embeds a random temp path).
lsGolden.push({ input: { cwd: lsRoot, args: { path: "afile" } }, expected: { ok: false, errorPrefix: "Not a directory:" } });
out.lsTool = lsGolden;

// --- skill formatting ---
const skill = (name, description, filePath, disableModelInvocation = false) => ({
  name, description, filePath, disableModelInvocation,
});
const skillCases = [
  [],
  [skill("deploy", "Deploy the app", "/skills/deploy/SKILL.md")],
  [
    skill("alpha", "First", "/skills/alpha/SKILL.md"),
    skill("beta", "Second", "/skills/beta/SKILL.md"),
    skill("gamma", "Third", "/skills/gamma/SKILL.md"),
  ],
  [
    skill("delta", "Fourth", "/skills/delta/SKILL.md"),
    skill("alpha", "First", "/skills/alpha/SKILL.md"),
    skill("beta", "Second", "/skills/beta/SKILL.md"),
    skill("gamma", "Third", "/skills/gamma/SKILL.md"),
  ],
  [
    skill("zeta", "Z", "/work/zeta/SKILL.md"),
    skill("alpha", "A", "/skills/alpha/SKILL.md"),
    skill("beta", "B", "/skills/beta/SKILL.md"),
    skill("eta", "H", "/work/eta/SKILL.md"),
    skill("theta", "T", "/work/theta/SKILL.md"),
  ],
  [
    skill("visible", "Shown", "/skills/visible/SKILL.md"),
    skill("hidden", "Not shown", "/skills/hidden/SKILL.md", true),
  ],
  [
    skill("xml<&>\"'", "desc & <tag> \"q\" 'a'", "/skills/x/SKILL.md"),
  ],
];
out.skillsPrompt = skillCases.map((skills) => ({
  input: { skills: skills.map((s) => ({
    name: s.name, description: s.description, filePath: s.filePath,
    disableModelInvocation: s.disableModelInvocation,
  })) },
  expected: core.formatAlkaidSkillsPrompt(skills),
}));

// --- agent loop (end-to-end with a mock LLM + mock tools) ---
const assistantMsg = (content, stopReason) => ({
  role: "assistant",
  content,
  api: "test-api", provider: "test-provider", model: "test-model",
  usage: { input: 1, output: 1, cacheRead: 0, cacheWrite: 0, totalTokens: 2, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
  stopReason,
  timestamp: 1000,
});
const textMsg = (text, stopReason = "end_turn") => assistantMsg([{ type: "text", text }], stopReason);
const toolCallMsg = (id, name, args) => assistantMsg([{ type: "toolCall", id, name, arguments: args }], "tool_calls");
const userMsg = (text) => ({ role: "user", content: [{ type: "text", text }], timestamp: 500 });

// Mock stream: yields start+done and resolves result() to the scripted message.
// A response entry may be a plain assistant message, or `{ deltas: [..], final: msg }`
// to exercise text_delta streaming with an accumulating partial message.
const buildStreamEvents = (entry) => {
  if (entry && entry.deltas) {
    const events = [];
    let text = "";
    const partial = () => ({ ...entry.final, content: [{ type: "text", text }] });
    events.push({ type: "start", partial: partial() });
    for (const delta of entry.deltas) {
      text += delta;
      events.push({ type: "text_delta", delta, partial: partial() });
    }
    events.push({ type: "done" });
    return { events, result: entry.final };
  }
  return {
    events: [
      { type: "start", partial: entry },
      { type: "done" },
    ],
    result: entry,
  };
};

const makeStreamFn = (responses) => {
  let call = 0;
  return async () => {
    const entry = responses[call] ?? textMsg("(no more responses)");
    call += 1;
    const { events, result } = buildStreamEvents(entry);
    return {
      async *[Symbol.asyncIterator]() {
        for (const event of events) yield event;
      },
      async result() { return result; },
    };
  };
};

const stripTimestamps = (value) => {
  if (Array.isArray(value)) return value.map(stripTimestamps);
  if (value && typeof value === "object") {
    const out = {};
    for (const [k, v] of Object.entries(value)) {
      if (k === "timestamp") continue;
      out[k] = stripTimestamps(v);
    }
    return out;
  }
  return value;
};

const runScenario = async ({ prompts, responses, tools, toolResults, toolExecution }) => {
  const context = {
    systemPrompt: "You are a test agent.",
    messages: [],
    tools: tools ?? [],
  };
  const config = {
    model: { id: "test-model", provider: "test-provider", api: "test-api" },
    convertToLlm: (messages) => messages.filter((m) => ["user", "assistant", "toolResult"].includes(m.role)),
    toolExecution: toolExecution ?? "sequential",
  };
  const events = [];
  const toolFn = (name, args) => {
    const result = toolResults?.[name] ?? { content: [{ type: "text", text: `ran ${name}` }], details: {} };
    return result;
  };
  // Wrap tools so execute() uses toolFn (pi execute signature: (id, args)).
  const wrappedTools = (tools ?? []).map((t) => ({ ...t, execute: async (_id, args) => toolFn(t.name, args) }));
  context.tools = wrappedTools;
  const streamFn = makeStreamFn(responses);
  const finalMessages = await runAgentLoop(prompts, context, config, async (event) => {
    events.push(event);
  }, undefined, streamFn);
  return { events: stripTimestamps(events), finalMessages: stripTimestamps(finalMessages) };
};

const agentScenarios = [
  { name: "plain_text", prompts: [userMsg("hello")], responses: [textMsg("hi there")], tools: [] },
  { name: "one_tool_call", prompts: [userMsg("echo hi")],
    responses: [toolCallMsg("call_1", "echo", { text: "hi" }), textMsg("done")],
    tools: [{ name: "echo", description: "echo", parameters: {} }],
    toolResults: { echo: { content: [{ type: "text", text: "echo: hi" }], details: { echoed: true } } } },
  { name: "unknown_tool", prompts: [userMsg("use ghost")],
    responses: [toolCallMsg("call_2", "ghost", {}), textMsg("recovered")],
    tools: [{ name: "echo", description: "echo", parameters: {} }] },
  { name: "error_stop", prompts: [userMsg("fail")],
    responses: [assistantMsg([{ type: "text", text: "" }], "error")], tools: [] },
  { name: "two_tool_calls", prompts: [userMsg("two")],
    responses: [assistantMsg([{ type: "toolCall", id: "a", name: "echo", arguments: { text: "1" } }, { type: "toolCall", id: "b", name: "echo", arguments: { text: "2" } }], "tool_calls"), textMsg("done")],
    tools: [{ name: "echo", description: "echo", parameters: {} }],
    toolResults: { echo: { content: [{ type: "text", text: "ok" }], details: {} } } },
  { name: "streaming_text", prompts: [userMsg("stream")],
    responses: [{ deltas: ["Hel", "lo, ", "world!"], final: textMsg("Hello, world!") }],
    tools: [] },
  { name: "parallel_two_tools", prompts: [userMsg("two parallel")],
    responses: [assistantMsg([{ type: "toolCall", id: "a", name: "echo", arguments: { text: "1" } }, { type: "toolCall", id: "b", name: "echo", arguments: { text: "2" } }], "tool_calls"), textMsg("done")],
    tools: [{ name: "echo", description: "echo", parameters: {} }],
    toolResults: { echo: { content: [{ type: "text", text: "ok" }], details: {} } },
    toolExecution: "parallel" },
];
out.agentLoop = [];
for (const scenario of agentScenarios) {
  const result = await runScenario(scenario);
  out.agentLoop.push({
    input: {
      name: scenario.name,
      systemPrompt: "You are a test agent.",
      prompts: scenario.prompts,
      responses: scenario.responses,
      tools: (scenario.tools ?? []).map((t) => ({ name: t.name })),
      toolResults: scenario.toolResults ?? {},
      toolExecution: scenario.toolExecution ?? "sequential",
    },
    expected: result,
  });
}

// --- Agent wrapper (stateful: steering queue + state reduction) ---
const runAgentClassScenario = async ({ promptText, responses, tools, toolResults, steerBefore }) => {
  const events = [];
  const toolFn = (name) => toolResults?.[name] ?? { content: [{ type: "text", text: `ran ${name}` }], details: {} };
  const wrappedTools = (tools ?? []).map((t) => ({ ...t, execute: async (_id, _args) => toolFn(t.name) }));
  const agent = new Agent({
    initialState: {
      systemPrompt: "You are a test agent.",
      model: { id: "test-model", provider: "test-provider", api: "test-api" },
      tools: wrappedTools,
      messages: [],
    },
    streamFn: makeStreamFn(responses),
    steeringMode: "all",
    toolExecution: "sequential",
  });
  agent.subscribe(async (event) => { events.push(event); });
  for (const message of steerBefore ?? []) agent.steer(message);
  await agent.prompt(promptText);
  return {
    events: stripTimestamps(events),
    finalMessages: stripTimestamps(agent.state.messages),
    errorMessage: agent.state.errorMessage ?? null,
  };
};

const agentClassScenarios = [
  { name: "agent_plain", promptText: "hello", responses: [textMsg("hi there")], tools: [] },
  { name: "agent_steering", promptText: "hello",
    steerBefore: [userMsg("steered input")],
    responses: [textMsg("response after steer")], tools: [] },
  { name: "agent_tool", promptText: "echo hi",
    responses: [toolCallMsg("call_1", "echo", { text: "hi" }), textMsg("done")],
    tools: [{ name: "echo", description: "echo", parameters: {} }],
    toolResults: { echo: { content: [{ type: "text", text: "echo: hi" }], details: {} } } },
];
out.agentClass = [];
for (const scenario of agentClassScenarios) {
  const result = await runAgentClassScenario(scenario);
  out.agentClass.push({
    input: {
      name: scenario.name,
      promptText: scenario.promptText,
      responses: scenario.responses,
      tools: (scenario.tools ?? []).map((t) => ({ name: t.name })),
      toolResults: scenario.toolResults ?? {},
      steerBefore: scenario.steerBefore ?? [],
    },
    expected: result,
  });
}

// --- bridge protocol translation (tool items) ---
const endItem = (started, endEvent) => {
  const output = endEvent.result?.content?.map((p) => p.text ?? "").join("\n") ?? "";
  return {
    ...started,
    status: endEvent.isError ? "failed" : "completed",
    aggregated_output: output,
    result: endEvent.isError ? undefined : endEvent.result,
    error: endEvent.isError ? { message: output } : undefined,
  };
};
const bridgeStartCases = [
  { toolCallId: "c1", toolName: "bash", args: { command: "ls -la" } },
  { toolCallId: "c2", toolName: "read", args: { path: "foo.txt" } },
  { toolCallId: "c3", toolName: "edit", args: { path: "a.rs", edits: [{ oldText: "x", newText: "y" }] } },
  { toolCallId: "c4", toolName: "edit_files", args: { files: [{ path: "a.rs", edits: [] }, { path: "b.rs", edits: [] }] } },
  { toolCallId: "c5", toolName: "write", args: { path: "new.txt", content: "hi" } },
  { toolCallId: "c6", toolName: "mcp__github__create_issue", args: { title: "bug" } },
  { toolCallId: "c7", toolName: "grep", args: { pattern: "foo" } },
  { toolCallId: "c8", toolName: "bash", args: null },
  { toolCallId: "c9", toolName: "edit_files", args: { files: "notarray" } },
];
out.bridgeToolItem = bridgeStartCases.map((event) => {
  const started = startedToolItem(event);
  const endEvent = { result: { content: [{ type: "text", text: "line1" }, { type: "text", text: "line2" }] }, isError: false };
  const completed = endItem(started, endEvent);
  const failed = endItem(started, { result: { content: [{ type: "text", text: "boom" }] }, isError: true });
  return { input: { event }, expected: { started, completed, failed } };
});

// --- single-file edit algorithm (edit-diff.js) ---
const editDiffCases = [
  { content: "alpha\nbeta\ngamma\n", edits: [["beta", "BETA"]], path: "a.txt" },
  { content: "fn main() {\n    let x = 1;\n}\n", edits: [["    let x = 1;", "    let y = 2;"]], path: "b.rs" },
  // fuzzy: trailing whitespace in content not in oldText
  { content: "keep me   \nother\n", edits: [["keep me", "changed"]], path: "c.txt" },
  // fuzzy: smart quotes in oldText, ascii in content
  { content: "say \"hi\" now\n", edits: [["say “hi” now", "say “bye” now"]], path: "d.txt" },
  // duplicate -> error
  { content: "foo\nbar\nfoo\n", edits: [["foo", "baz"]], path: "e.txt" },
  // not found -> error
  { content: "abc\n", edits: [["xyz", "q"]], path: "f.txt" },
  // empty oldText -> error
  { content: "abc\n", edits: [["", "q"]], path: "g.txt" },
  // no change -> error
  { content: "same\n", edits: [["same", "same"]], path: "h.txt" },
  // overlap -> error
  { content: "aaaa bbbb cccc\n", edits: [["aaaa bbbb", "X"], ["bbbb cccc", "Y"]], path: "i.txt" },
  // multiple non-overlapping
  { content: "one\ntwo\nthree\n", edits: [["one", "ONE"], ["three", "THREE"]], path: "j.txt" },
  // CJK exact
  { content: "第一行\n第二行\n", edits: [["第二行", "改"]], path: "k.txt" },
  // fuzzy preserves unchanged line bytes (trailing ws on a non-edited line stays)
  { content: "lineA   \nlineB\nlineC   \n", edits: [["lineB", "lineB2"]], path: "l.txt" },
];
out.editDiff = editDiffCases.map(({ content, edits, path }) => {
  try {
    const result = applyEditsToNormalizedContent(content, edits.map(([oldText, newText]) => ({ oldText, newText })), path);
    return { input: { content, edits, path }, expected: { ok: true, baseContent: result.baseContent, newContent: result.newContent } };
  } catch (error) {
    return { input: { content, edits, path }, expected: { ok: false, error: error instanceof Error ? error.message : String(error) } };
  }
});

writeFileSync("src-tauri/pi_core/testdata/golden.json", JSON.stringify(out));
console.log("wrote src-tauri/pi_core/testdata/golden.json", JSON.stringify(out).length, "bytes");
for (const k of Object.keys(out)) console.log(" ", k, out[k].length);
