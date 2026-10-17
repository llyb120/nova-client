import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import ts from "typescript";

// Run the actual option/codec code without mounting the Tauri/Solid application.
const source = ts.createSourceFile("ConfigSelects.tsx", readFileSync(
  new URL("../src/components/ConfigSelects.tsx", import.meta.url), "utf8",
), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
const names = new Set([
  "encodeModelValue", "decodeModelValue", "modelOptionsOf",
  "kinds", "merged", "lyraProviders", "backendOptions", "sourceOf", "modelOptions",
  "fmt", "creditsText", "priceText", "multiplierText", "detailTitle",
]);
const units = [];
function visit(node) {
  if ((ts.isVariableDeclaration(node) || ts.isFunctionDeclaration(node)) && node.name && names.has(node.name.getText(source))) {
    units.push(ts.isVariableDeclaration(node) ? `const ${node.getText(source)};` : node.getText(source));
    return;
  }
  ts.forEachChild(node, visit);
}
visit(source);
assert.equal(units.length, names.size);
const code = ts.transpileModule(units.join("\n").replace(/export function/g, "function"), {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None },
}).outputText;
const kinds = ["lyra", "devin", "codex", "codebuddy", "workbuddy", "cursor", "kimi", "claude"];
const efforts = {};
new Function("exports", ts.transpileModule(readFileSync(
  new URL("../src/modelEfforts.ts", import.meta.url), "utf8",
), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText)(efforts);
const select = new Function("props", "ALL_AGENT_KINDS", "foldModelEfforts", "splitEffort", `
  const createMemo = f => f;
  const state = { modelCosts: props.modelCosts };
  const agentLabel = k => k;
  const modelChoices = (kind, source) => source ?? [];
  const groupOf = () => "models";
  ${code}
  return {
    options: modelOptions,
    backends: backendOptions,
    decode: decodeModelValue,
    credits: creditsText,
    models: modelOptionsOf,
  };
`);

test("WorkBuddy multipliers appear on models and folded reasoning levels, including free models", () => {
  const picker = select({}, kinds, efforts.foldEfforts, efforts.splitEffort);
  for (const [description, expected] of [
    ["x0.29", "0.29×"], ["x1.20", "1.2×"], ["x0.00", "0×"],
    ["  X0.65  ", "0.65×"], ["×0.21", "0.21×"],
    ["x0.79 credits", "0.79×"], ["0 credits", "0×"],
    [undefined, undefined], ["", undefined], ["128k context", undefined],
    ["context x2", undefined], ["x0.2.3", undefined], ["x-1", undefined],
  ]) assert.equal(picker.credits(description), expected, String(description));
  for (const [base, description, detail] of [["hy4-preview", "x0.29", "0.29×"], ["hy3", "x0.00", "0×"]]) {
    const [model] = picker.models("workbuddy", false,
      ["low", "high"].map(effort => ({ value: `${base}:${effort}`, name: `${base} · ${effort}`, description })), true);
    assert.equal(model.detail, detail);
    assert.match(model.detailTitle, /积分倍率/);
    assert.equal(model.efforts.length, 2);
    for (const effort of model.efforts) assert.equal(effort.detail, detail);
  }
});

test("Devin reasoning levels inherit base prices without overriding exact model prices", () => {
  const picker = select({ modelCosts: {
    "devin-model": { prices: { input: 5, cached: 0.5, output: 25 } },
    "devin-model:max": { prices: { input: 6, cached: 0.6, output: 30 } },
    free: { prices: { input: 0, cached: 0, output: 0 } },
  } }, kinds, efforts.foldEfforts, efforts.splitEffort);
  const choices = ["low", "high", "max"].map(effort => ({
    value: `devin-model:${effort}`, name: `Model · ${effort}`,
  }));
  for (const merged of [false, true]) {
    const flat = picker.models("devin", merged, choices);
    assert.deepEqual(flat.map(o => o.detail), ["$5/$25", "$5/$25", "$6/$30"]);
    assert.match(flat[0].detailTitle, /缓存命中 \$0\.5/);
    const [parent] = picker.models("devin", merged, choices, true);
    assert.equal(parent.detail, "$5/$25");
    assert.deepEqual(parent.efforts.map(o => o.detail), flat.map(o => o.detail));
  }
  const [free, unknown, invalidSuffix] = picker.models("devin", false, [
    { value: "free:high", name: "Free" },
    { value: "missing:high", name: "Unknown" },
    { value: "devin-model:not-an-effort", name: "Other" },
  ]);
  assert.equal(free.detail, "$0/$0");
  assert.equal(unknown.detail, undefined);
  assert.equal(invalidSuffix.detail, undefined);
  assert.equal(picker.models("workbuddy", false, [choices[0]])[0].detail, undefined);
});

test("expanded Lyra providers keep the selected effort in its model's backend", () => {
  const choices = {
    lyra: ["local-claude-code", "other-provider"].flatMap(provider => [
      { value: `${provider}/claude/sonnet`, name: `${provider} / Sonnet` },
      { value: `${provider}/claude/sonnet/variant/high`, name: `${provider} / Sonnet · High` },
    ]),
    codex: [{ value: "gpt:high", name: "GPT · High" }],
  };
  for (const merged of [true, false]) {
    const picker = select({
      agentKind: "lyra",
      ...(merged ? { agentKinds: ["lyra", "codex"] } : {}),
      modelSource: kind => choices[kind],
    }, kinds, efforts.foldEfforts, efforts.splitEffort);
    const options = picker.options();
    assert.equal(options.length, merged ? 3 : 2);
    for (const model of options) {
      const kind = model.title.startsWith("gpt") ? "codex" : "lyra";
      const expectedBackend = merged
        ? kind === "lyra" ? `lyra:${model.title.split("/")[0]}` : kind
        : undefined;
      assert.equal(model.backend, expectedBackend);
      if (merged) assert.ok(picker.backends().some(b => b.id === model.backend));
      for (const effort of model.efforts) {
        assert.equal(effort.backend, model.backend, "reopening a selected effort must find the parent provider");
        assert.equal(effort.group, model.group);
        assert.equal(effort.favoriteId, `${kind}:${encodeURIComponent(effort.title)}`);
        assert.deepEqual(merged ? picker.decode(effort.value) : effort.value,
          merged ? { agentKind: kind, model: effort.title } : effort.title);
      }
    }
  }
});
