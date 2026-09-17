import assert from "node:assert/strict";
import { processSegments, processSummary, processLiveLines, splitTurnBody } from "../src/processDisplay.ts";

const thought = (id, text = "分析\n下一步") => ({ type: "thought", id, text });
const tool = (id, kind, status = "completed") => ({ type: "tool", id, kind, status, title: `${kind}\nfile.ts` });
const items = [thought(1), tool(2, "read"), tool(3, "edit"), { type: "assistant", id: 4, text: "进展" }, thought(5)];
const segments = processSegments(items);
assert.deepEqual(segments.map(s => [s.type, s.id]), [["process", 1], ["item", 4], ["process", 5]]);
assert.deepEqual(segments.flatMap(s => s.type === "process" ? s.items : [s.item]), items);
assert.equal(processSummary(segments[0].items), "读取文件 ×1、修改文件 ×1");
assert.equal(processSummary([tool(1, "read"), tool(2, "read", "failed")]), "读取文件 ×2（1 项失败）");
assert.equal(processSummary([tool(1, "execute"), tool(2, "execute"), tool(3, "execute")]), "执行命令 ×3");
assert.equal(processSummary([tool(1, "execute"), tool(2, "read"), tool(3, "execute")]), "执行命令 ×2、读取文件 ×1");
assert.equal(processSummary([thought(1)]), "分析思路");
assert.deepEqual(processLiveLines(segments[0].items), ["已完成 · read file.ts", "已完成 · edit file.ts"]);
assert.deepEqual(processLiveLines([thought(1), tool(2, "search", "in_progress")]), ["下一步", "进行中 · search file.ts"]);
assert.deepEqual(processLiveLines([tool(1, "read"), thought(2, "开始思考")]), ["已完成 · read file.ts", "开始思考"]);
assert.deepEqual(processLiveLines([tool(1, "read"), thought(2, "开始思考\n继续分析")]), ["开始思考", "继续分析"]);
assert.deepEqual(processLiveLines([thought(1, "上一段"), thought(2, "新段落")]), ["上一段", "新段落"]);
const wrapThree = text => text.match(/.{1,3}/gu) ?? [];
assert.deepEqual(processLiveLines([tool(1, "read"), thought(2, "abc")], wrapThree), [".ts", "abc"]);
assert.deepEqual(processLiveLines([tool(1, "read"), thought(2, "abcd")], wrapThree), ["abc", "d"]);
assert.deepEqual(processLiveLines([tool(1, "read"), thought(2, "旧行\n最新第一行\n最新第二行\n")]), ["最新第一行", "最新第二行"]);
assert.deepEqual(processLiveLines([thought(1, "abcdef")], text => text.match(/.{1,2}/gu)), ["cd", "ef"]);
assert.deepEqual(processLiveLines([thought(1, "abcdefg")], text => text.match(/.{1,2}/gu)), ["ef", "g"]);
assert.deepEqual(processLiveLines([]), []);
assert.deepEqual(processSegments([]), []);
const reply = (id, text) => ({type: "assistant", id, text});
const memory = (id, kind = "edit", path = "C:\\Users\\test\\.codebuddy\\memories\\iterations.md") =>
  ({...tool(id, kind), rawInput: {file_path: path}, locations: []});
const body = [reply(10, "我来查询"), tool(11, "read"), reply(12, "| 需求 | 优先级 |\n|---|---|\n| A | P1 |"),
  thought(13), memory(14, "read"), memory(15), reply(16, "备忘已记好。")];
assert.deepEqual(splitTurnBody(body, true).conclusion.map(it => it.id), [12, 16]);
assert.deepEqual(splitTurnBody(body, true).process.map(it => it.id), [10, 11, 13, 14, 15]);
assert.deepEqual(splitTurnBody(body, false), {process: body, conclusion: []});
for (const middle of [tool(15, "edit"), memory(15, "read"), {...memory(15), status: "failed"},
  memory(15, "execute"), {...memory(15), locations: [{path: "src/app.ts"}]}]) {
  assert.deepEqual(splitTurnBody([body[2], middle, body.at(-1)], true).conclusion.map(it => it.id), [16]);
}
assert.deepEqual(splitTurnBody([body[2], memory(15), tool(17, "read"), body.at(-1)], true).conclusion.map(it => it.id), [16]);
assert.deepEqual(splitTurnBody([body[2], memory(15), body.at(-1), tool(17, "edit")], true).conclusion.map(it => it.id), [16]);
assert.deepEqual(splitTurnBody([body[2], memory(15), reply(16, "详细的新结果".repeat(100))], true).conclusion.map(it => it.id), [16]);
assert.deepEqual(splitTurnBody([reply(1, "结果"), reply(2, "补充")], true).conclusion.map(it => it.id), [1, 2]);
assert.deepEqual(splitTurnBody([body[2], memory(15)], true).conclusion.map(it => it.id), [12]);
assert.deepEqual(splitTurnBody([body[2], {...tool(15, "other"), title: "save_memory"}, body.at(-1)], true).conclusion.map(it => it.id), [12, 16]);
assert.deepEqual(splitTurnBody([], true), {process: [], conclusion: []});
console.log("process display checks passed");
