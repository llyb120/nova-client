import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import ts from "typescript";

// Run the actual option/codec code without mounting the Tauri/Solid application.
const source = ts.createSourceFile("ConfigSelects.tsx", readFileSync(
  new URL("../src/components/ConfigSelects.tsx", import.meta.url), "utf8",
), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
const names = new Set([
  "encodeModelValue", "encodeQuotaModelValue", "decodeModelValue", "modelOptionsOf", "sharedList",
  "kinds", "sharedReady", "merged", "lyraProviders", "backendOptions", "sourceOf", "modelOptions",
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
const kinds = ["lyra", "devin", "codex", "codebuddy", "cursor", "kimi", "claude"];
const efforts = {};
new Function("exports", ts.transpileModule(readFileSync(
  new URL("../src/modelEfforts.ts", import.meta.url), "utf8",
), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText)(efforts);
const select = new Function("props", "sharedOnly", "ALL_AGENT_KINDS", "foldModelEfforts", "splitEffort", `
  const createMemo = f => f;
  const state = {};
  const agentLabel = k => k;
  const modelChoices = (kind, source) => source ?? [];
  const groupOf = () => "models";
  const priceText = () => undefined;
  const multiplierText = priceText, creditsText = priceText, detailTitle = priceText;
  ${code}
  return {
    shared: () => sharedList().flatMap(option => option.efforts ?? [option])
      .map(option => ({ option, decoded: decodeModelValue(option.value) })),
    options: modelOptions,
    backends: backendOptions,
    decode: decodeModelValue,
  };
`);

test("shared selection preserves the exact model ID for every backend", () => {
  const peer = { token: "team:user/%", name: "队友" };
  for (const kind of kinds) {
    for (const model of ["provider/model", "provider/model:high", "模型 / 100%", "gpt-5.6"]) {
      const props = { agentKind: kind, sharedModels: [{ peer, options: { [kind]: [{ value: model, name: model }] } }] };
      const [{ option, decoded }] = select(props, () => false, kinds, efforts.foldEfforts, efforts.splitEffort).shared();
      assert.deepEqual(decoded, { agentKind: kind, model, peerToken: peer.token });
      assert.equal(option.favoriteId, option.value);
      assert.equal(select(props, () => true, kinds, efforts.foldEfforts, efforts.splitEffort).shared()[0].option.value, model);
    }
  }
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
    }, () => false, kinds, efforts.foldEfforts, efforts.splitEffort);
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
