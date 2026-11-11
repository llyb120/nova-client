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
  "kinds", "merged", "sourceOf", "modelOptions",
  "fmt", "creditsText", "modelNameOf", "splitVendor", "latestModels", "NEUTRAL_WORDS",
  "sortModelOptions",
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
const kinds = ["lyra"];
const efforts = {};
new Function("exports", ts.transpileModule(readFileSync(
  new URL("../src/modelEfforts.ts", import.meta.url), "utf8",
), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText)(efforts);
const select = new Function("props", "ALL_AGENT_KINDS", "foldModelEfforts", "splitEffort", `
  const createMemo = f => f;
  const state = { modelCosts: props.modelCosts };
  const agentLabel = k => k;
  const isSupportedAgentKind = kind => ALL_AGENT_KINDS.includes(kind);
  const modelChoices = (kind, source) => source ?? [];
  const modelFavoriteIds = () => props.favorites ?? [];
  ${code}
  return {
    options: modelOptions,
    decode: decodeModelValue,
    credits: creditsText,
    models: modelOptionsOf,
    latest: latestModels,
    group: modelNameOf,
    vendor: (name) => splitVendor(name)[0],
  };
`);

test("Lyra local provider multipliers appear on models and folded reasoning levels, including free models", () => {
  const picker = select({}, kinds, efforts.foldEfforts, efforts.splitEffort);
  for (const [description, expected] of [
    ["x0.29", "0.29×"], ["x1.20", "1.2×"], ["x0.00", "0×"],
    ["  X0.65  ", "0.65×"], ["×0.21", "0.21×"],
    ["x0.79 credits", "0.79×"], ["0 credits", "0×"],
    [undefined, undefined], ["", undefined], ["128k context", undefined],
    ["context x2", undefined], ["x0.2.3", undefined], ["x-1", undefined],
  ]) assert.equal(picker.credits(description), expected, String(description));
  for (const [base, description, detail] of [["hy4-preview", "x0.29", "0.29×"], ["hy3", "x0.00", "0×"]]) {
    const [model] = picker.models("lyra", false,
      ["low", "high"].map(effort => ({ value: `workbuddy/${base}/variant/${effort}`, name: `${base} · ${effort}`, description })), true);
    assert.equal(model.detail, detail);
    assert.match(model.detailTitle, /积分倍率/);
    assert.equal(model.efforts.length, 2);
    for (const effort of model.efforts) assert.equal(effort.detail, detail);
  }
});

test("展开成行的强度项 label 自带档位，后端名字没带强度时也补上", () => {
  const picker = select({}, kinds, efforts.foldEfforts, efforts.splitEffort);
  const [model] = picker.models("lyra", false,
    [{ value: "workbuddy/hy4/variant/high", name: "WorkBuddy / Hy4" }], true);
  assert.equal(model.efforts.length, 1);
  assert.equal(model.efforts[0].label, "WorkBuddy / Hy4 · high");
});

test("removed standalone backends do not become model picker options", () => {
  const picker = select({}, kinds, efforts.foldEfforts, efforts.splitEffort);
  for (const kind of ["devin", "codebuddy", "codebuddy-plus", "workbuddy"]) {
    assert.deepEqual(picker.models(kind, true, [{ value: "old-model", name: "Old model" }]), []);
    assert.equal(picker.decode(`${kind}:old-model`), null);
  }
});

test("grouping uses the model name and the picker shows supplier + full name", () => {
  const choices = {
    lyra: ["local-claude-code", "other-provider"].flatMap(provider => [
      { value: `${provider}/claude/sonnet`, name: `${provider} / Sonnet` },
      { value: `${provider}/claude/sonnet/variant/high`, name: `${provider} / Sonnet · High` },
    ]),
  };
  for (const merged of [true, false]) {
    const picker = select({
      agentKind: "lyra",
      ...(merged ? { agentKinds: ["lyra"] } : {}),
      modelSource: kind => choices[kind],
    }, kinds, efforts.foldEfforts, efforts.splitEffort);
    const options = picker.options();
    assert.equal(options.length, 2);
    for (const model of options) {
      const kind = "lyra";
      assert.equal(model.group, "Sonnet", "first column is the model name, not the vendor");
      assert.equal(model.backend, undefined);
      for (const effort of model.efforts) {
        assert.equal(effort.group, model.group);
        assert.equal(effort.favoriteId, `${kind}:${encodeURIComponent(effort.title)}`);
        assert.deepEqual(merged ? picker.decode(effort.value) : effort.value,
          merged ? { agentKind: kind, model: effort.title } : effort.title);
      }
    }
  }
});

test("供应商与模型名拆分：供应商名自带 / 时从右往左取，强度后缀不算名字，连字符归一成空格", () => {
  const picker = select({}, kinds, efforts.foldEfforts, efforts.splitEffort);
  for (const [name, expected] of [
    ["Command Code / Claude Haiku 4.5", "Claude Haiku 4.5"],
    ["CodeBuddy / WorkBuddy / GLM-5.3 · High", "GLM 5.3"],
    ["MiniMax-M3.1-Flash", "MiniMax M3.1 Flash"],
    // 不同供应商的同一模型写法归一，才能归到同一组
    ["CodeBuddy / WorkBuddy / Kimi-K3.1-Pro · High", "Kimi K3.1 Pro"],
    ["Command Code / Kimi K3.1 Pro · High", "Kimi K3.1 Pro"],
  ]) assert.equal(picker.group(name), expected, name);
  // 同一模型名下的第二屏按供应商分段，供应商名本身带 / 时取左边那一整段
  for (const [name, expected] of [
    ["CodeBuddy / DeepSeek-V4.1-Flash · High", "CodeBuddy"],
    ["CodeBuddy / WorkBuddy / GLM-5.3 · High", "CodeBuddy / WorkBuddy"],
    ["Devin（本地 API） / GPT-5.5 · High", "Devin（本地 API）"],
    ["MiniMax-M3.1-Flash", ""],
  ]) assert.equal(picker.vendor(name), expected, name);
});

test("供应商的「默认模型」占位项不列，正选着时保留", () => {
  const picker = select({}, kinds, efforts.foldEfforts, efforts.splitEffort);
  const choices = [{ value: "codebuddy/codebuddy", name: "CodeBuddy / WorkBuddy / CodeBuddy（默认模型）" }];
  assert.deepEqual(picker.models("lyra", false, choices, true), []);
  assert.equal(picker.models("lyra", false, choices, true, "codebuddy/codebuddy").length, 1);
});

test("不列的型号：Devin 跟随 CLI 默认、gpt-live / gpt-image 线、grok-imagine、gpt-5.5，正选着时保留", () => {
  const picker = select({}, kinds, efforts.foldEfforts, efforts.splitEffort);
  for (const name of [
    "Devin（跟随 CLI 默认模型）",
    "OpenAI 兼容 / gpt-live",
    "OpenAI 兼容 / gpt-image-2-2k",
    "OpenAI 兼容 / gpt-live-1-mini",
    "OpenAI 兼容 / grok-imagine-image-quality",
    "OpenAI 兼容 / Grok Imagine Video 1.5 Preview",
    "Devin（本地 API） / GPT-5.5 · High",
    "OpenAI 兼容 / GPT-5.5 · Low",
  ]) {
    const choices = [{ value: name, name }];
    assert.deepEqual(picker.models("lyra", false, choices, true), [], name);
    assert.equal(picker.models("lyra", false, choices, true, name).length, 1, name);
  }
  // 相近但不该被误伤的型号
  for (const name of [
    "Devin（本地 API） / GPT-5.6 Sol · High",
    "OpenAI 兼容 / grok-4.7",
    "OpenAI 兼容 / gpt-5.4",
  ]) assert.equal(picker.models("lyra", false, [{ value: name, name }], true).length, 1, name);
});

test("同系列只留最高版本：带规格词也算同系列，选中项和收藏不被隐藏", () => {
  const picker = select({ favorites: ["lyra:keep%2Fsonnet"], agentKind: "lyra", agentKinds: ["lyra"] },
    kinds, efforts.foldEfforts, efforts.splitEffort);
  const names = [
    "Claude Sonnet 5.5", "Claude Sonnet 5", "Claude Sonnet 4.6",
    "Devin（本地 API） / Claude Sonnet 4.6 Thinking", "Devin（本地 API） / Claude Sonnet 4.6 1M",
    "Kimi K3.1 Pro", "Kimi K3", "Kimi K2.7 Code",
    "Claude Haiku 5.5 Flash", "Inkling", "Inkling Small",
  ];
  const options = names.map(name => ({
    value: name,
    label: name,
    title: name,
    group: picker.group(name),
    favoriteId: name === "Claude Sonnet 4.6" ? "lyra:keep%2Fsonnet" : undefined,
  }));
  const kept = picker.latest(options).map(o => o.label);
  // 4.6 因为被收藏而保留；K2.7 Code 是另一条变体线，不是 K3.1 Pro 的旧版本
  assert.deepEqual(kept, [
    "Claude Sonnet 5.5",
    "Claude Sonnet 4.6",
    "Kimi K3.1 Pro",
    "Kimi K2.7 Code",
    "Claude Haiku 5.5 Flash",
    "Inkling",
    "Inkling Small",
  ]);
  // 当前选中的旧版本、以及收藏过的旧版本仍要能选到
  assert.ok(picker.latest(options, "Claude Sonnet 4.6").some(o => o.label === "Claude Sonnet 4.6"));
  assert.ok(picker.latest(options).some(o => o.favoriteId === "lyra:keep%2Fsonnet"));
});

test("model picker lists are sorted case-insensitively by display name and keep the current selection", () => {
  const picker = select({}, kinds, efforts.foldEfforts, efforts.splitEffort);
  const current = "provider/zeta";
  const options = picker.models("lyra", false, [
    { value: "provider/zeta", name: "Vendor / zeta" },
    { value: "provider/beta", name: "Vendor / Beta" },
    { value: "provider/alpha", name: "Vendor / alpha" },
    { value: "provider/gamma", name: "other / Gamma" },
  ], false, current);
  assert.deepEqual(options.map(o => o.value), [
    "provider/alpha",
    "provider/beta",
    "provider/gamma",
    "provider/zeta",
  ]);
  assert.ok(options.some(o => o.value === current));
});

