import assert from "node:assert";
import { foldEfforts } from "../src/modelEfforts.ts";

const opt = (value: string, label: string) => ({ value, label, title: value });
const fold = (flat: ReturnType<typeof opt>[], lyra: boolean) => foldEfforts(flat, lyra, (v) => v, (v) => `k:${v}`);

// ACP：default 档存在时父项提交它；裸模型被父项取代；强度短名取 " · " 之后。
const acp = fold([
  opt("opus", "Opus"),
  opt("opus:default", "Opus · 默认"),
  opt("opus:high", "Opus · High"),
  opt("haiku", "Haiku"),
], false);
assert.deepEqual(acp.map((o) => [o.value, o.label]), [["opus:default", "Opus"], ["haiku", "Haiku"]]);
assert.deepEqual(acp[0].efforts!.map((o) => o.short), ["默认", "High"]);
assert.equal(acp[0].favoriteId, "k:opus");

// Lyra：只按 /variant/ 拆，模型 ID 里的 :max 不当强度；无 default 档时父项为基础模型。
const lyra = fold([
  opt("p/qwen3:max", "Qwen3 Max"),
  opt("p/gpt/variant/low", "GPT · low"),
  opt("p/gpt/variant/high", "GPT · high"),
  opt("p/gpt", "GPT"),
], true);
assert.deepEqual(lyra.map((o) => [o.value, o.efforts?.length]), [["p/qwen3:max", undefined], ["p/gpt", 2]]);
console.log("ok");
