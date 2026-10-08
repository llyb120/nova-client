import assert from "node:assert";
import { foldEfforts } from "../src/modelEfforts.ts";

const opt = (value: string, label: string) => ({ value, label, title: value });
const fold = (flat: ReturnType<typeof opt>[], lyra: boolean, defaults = new Map<string, string>()) =>
  foldEfforts(flat, lyra, (v) => v, (v) => `k:${v}`, defaults);

// ACP：default 档存在时父项提交它；裸模型被父项取代；强度短名取 " · " 之后。
const acp = fold([
  opt("opus", "Opus"),
  opt("opus:default", "Opus · 默认"),
  opt("opus:high", "Opus · High"),
  opt("haiku", "Haiku"),
], false, new Map([["opus", "high"]]));
assert.deepEqual(acp.map((o) => [o.value, o.label]), [["opus:default", "Opus"], ["haiku", "Haiku"]]);
assert.deepEqual(acp[0].efforts!.map((o) => o.short), ["默认", "High"]);
assert.equal(acp[0].favoriteId, "k:opus");
assert.equal(acp[0].selectedLabel, "Opus · High");
assert.equal(acp[0].efforts![0].short, "默认", "默认选项仍保持默认语义");

// Lyra：只按 /variant/ 拆，模型 ID 里的 :max 不当强度；无 default 档时父项为基础模型。
const lyra = fold([
  opt("p/qwen3:max", "Qwen3 Max"),
  opt("p/gpt/variant/low", "GPT · low"),
  opt("p/gpt/variant/high", "GPT · high"),
  opt("p/gpt", "GPT"),
], true);
assert.deepEqual(lyra.map((o) => [o.value, o.efforts?.length]), [["p/qwen3:max", undefined], ["p/gpt", 2]]);
assert.equal(lyra[0].selectedLabel, undefined, "没有档位的模型不追加强度");
assert.equal(lyra[1].selectedLabel, "GPT · 默认", "未知默认强度不能猜测");

// 截图中的 CodeBuddy 裸模型默认，以及 Lyra 的模型/variant 编码。
for (const isLyra of [false, true]) {
  const base = isLyra ? "provider/model" : "deepseek-v4.1-flash";
  const variant = (effort: string) => isLyra ? `${base}/variant/${effort}` : `${base}:${effort}`;
  const options = [opt(base, "Model"), opt(variant("low"), "Model · Low"), opt(variant("high"), "Model · High")];
  for (const ordered of [options, [...options].reverse()]) {
    const [parent] = fold(ordered, isLyra, new Map([[base, "high"]]));
    assert.equal(parent.value, base, "显示实际强度不改变默认提交值");
    assert.equal(parent.label, "Model");
    assert.equal(parent.selectedLabel, "Model · High");
    assert.equal(parent.efforts!.find((o) => o.value === variant("low"))!.label, "Model · Low");
  }
}
console.log("ok");
