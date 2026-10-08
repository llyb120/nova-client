// Run: node scripts/stage-submit.test.mjs
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { transformSync } from "esbuild";

const read = (path) => readFileSync(new URL(path, import.meta.url), "utf8");
const store = read("../src/store.ts");
const stage = store.slice(store.indexOf("export const STAGE_COMMAND_PATTERN"), store.indexOf("async function startStageThread("));
const module = { exports: {} };
new Function("module", "exports", transformSync(`${stage}\nexport { parseStageInput };`, {
  loader: "ts", format: "cjs",
}).code)(module, module.exports);
const { STAGE_COMMAND_PATTERN, parseStageInput } = module.exports;

const composer = read("../src/components/Composer.tsx");
const submitSource = composer.slice(composer.indexOf("  const shouldQueue"), composer.indexOf("  const insertSlashSuggestion"));
const createSubmit = new Function("deps", transformSync(`
  const { running, STAGE_COMMAND_PATTERN, text, empty, attach, state,
    rememberPromptHistory, clearInput, enqueuePrompt, dropQueuedPromptsMatching,
    sendPrompt, releasePromptQueue } = deps;
  ${submitSource}
  return submit;
`, { loader: "ts" }).code);

for (const busy of [true, false]) {
  for (const [input, isStage] of [
    ["/stage 复核方案", true],
    ["  /STAGE2\t复核方案  ", true],
    ["补充当前任务\r\n/stage3\n独立复核", true],
    ["补充当前任务 /stage 复核方案", true],
    ["继续执行", false],
    ["/stages 只是文本", false],
    ["docs/stage 文件路径", false],
    ["/stage2x 普通文本", false],
  ]) {
    let value = input;
    const calls = [];
    const noop = () => {};
    createSubmit({
      running: () => busy, STAGE_COMMAND_PATTERN,
      text: () => value, empty: () => !value.trim(), attach: { images: () => [] },
      state: { currentId: "source" }, rememberPromptHistory: noop,
      clearInput: () => { value = ""; },
      enqueuePrompt: (id, text) => calls.push(["queue", text]),
      dropQueuedPromptsMatching: noop, releasePromptQueue: noop,
      sendPrompt: async (text) => { calls.push(["send", text]); },
    })();
    assert.deepEqual(calls, [[busy && !isStage ? "queue" : "send", input.trim()]], input);
    assert.equal(!!parseStageInput(input.trim()), isStage, "routing and parsing must agree");
  }
}
assert.deepEqual(parseStageInput("补充当前任务\n/stage2 复核方案"), {
  currentPrompt: "补充当前任务", stagePrompt: "复核方案", stageIndex: 1,
});
assert.throws(() => parseStageInput("/stage"), /新会话提示词/);
assert.throws(() => parseStageInput("/stage0 复核方案"), /编号必须从 1 开始/);
console.log("Stage submits immediately while busy; ordinary prompts keep their queue behavior.");
