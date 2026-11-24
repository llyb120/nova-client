import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import ts from "typescript";

// 不挂载 Solid/Tauri：直接跑 src/modelEfforts.ts 的档位换算。
const js = ts.transpileModule(
  readFileSync(new URL("../src/modelEfforts.ts", import.meta.url), "utf8"),
  { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None } },
).outputText;
const { stepEffort, foldEfforts, acpVariants } = new Function("exports", `${js}\nreturn { stepEffort, foldEfforts, acpVariants };`)({});

test("ACP parenthesized effort levels fold without changing wire IDs", () => {
  const base = 'gemini-4-hydrogen';
  const choices = ['XHigh', 'Low', 'Medium', 'High'].map(e => ({value: `${base}-${e.toLowerCase()}`, name: `Gemini 4 Hydrogen (${e})`}));
  choices.push({ value: base, name: 'Gemini 4 Hydrogen' }, { value: 'other-medium', name: 'Other (Medium)' });
  const variants = acpVariants(choices);
  const flat = choices.map(m => ({value: m.value, title: m.value, label: variants.has(m.value) ? m.name.replace(/ \((\w+)\)$/, ' · $1') : m.name}));
  const [parent, other] = foldEfforts(flat, false, v => v, v => v, new Map([[base, 'medium']]), variants);
  assert.equal(other.value, 'other-medium');
  assert.equal(parent.title, base);
  assert.equal(parent.label, 'Gemini 4 Hydrogen');
  assert.equal(parent.selectedLabel, 'Gemini 4 Hydrogen · Medium');
  assert.deepEqual(parent.efforts.map(e => e.value), ['low', 'medium', 'high', 'xhigh'].map(e => `${base}-${e}`));
});

const effort = (name) => ({ value: `p/m/variant/${name}`, label: `M · ${name}`, title: `p/m/variant/${name}` });
const model = (selectedLabel) => ({
  value: "p/m",
  label: "M",
  title: "p/m",
  selectedLabel,
  efforts: [effort("low"), effort("high"), effort("max")],
});

test("Alt+滚轮在强度档之间移动，两端不越界", () => {
  const options = [model("M · high")];
  assert.equal(stepEffort("p/m/variant/high", options, 1), "p/m/variant/max");
  assert.equal(stepEffort("p/m/variant/high", options, -1), "p/m/variant/low");
  assert.equal(stepEffort("p/m/variant/max", options, 1), null);
  assert.equal(stepEffort("p/m/variant/low", options, -1), null);
  // 裸模型（后端默认强度）从选择器标注的默认档起算。
  assert.equal(stepEffort("p/m", options, 1), "p/m/variant/max");
  assert.equal(stepEffort("p/m", options, -1), "p/m/variant/low");
});

test("没有强度档、只有一档或不是当前模型时不切换", () => {
  assert.equal(stepEffort("x/y", [{ value: "x/y", label: "Y", title: "x/y" }], 1), null);
  const single = [{ value: "x/y", label: "Y", title: "x/y", selectedLabel: "Y", efforts: [effort("high")] }];
  assert.equal(stepEffort("x/y", single, 1), null);
  assert.equal(stepEffort("x/y", [], 1), null);
  assert.equal(stepEffort("other/model", [model("M · high")], 1), null);
});
