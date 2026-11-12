import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import ts from "typescript";

// 不挂载 Solid/Tauri：直接跑 src/modelEfforts.ts 的档位换算。
const js = ts.transpileModule(
  readFileSync(new URL("../src/modelEfforts.ts", import.meta.url), "utf8"),
  { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None } },
).outputText;
const { stepEffort } = new Function("exports", `${js}\nreturn { stepEffort };`)({});

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
