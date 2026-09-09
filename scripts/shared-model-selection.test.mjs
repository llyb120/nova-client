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
]);
const units = [];
function visit(node) {
  if (node.name && names.has(node.name.getText(source))) {
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
const kinds = ["lyra", "devin", "codex", "codebuddy", "claudecode", "cursor", "opencode"];
const select = new Function("props", "sharedOnly", "ALL_AGENT_KINDS", `
  const createMemo = f => f;
  const state = {};
  const agentLabel = k => k;
  const modelChoices = (kind, source) => source ?? [];
  const groupOf = () => "models";
  const priceText = () => undefined;
  const multiplierText = priceText, creditsText = priceText, detailTitle = priceText;
  ${code}
  return sharedList().map(option => ({ option, decoded: decodeModelValue(option.value) }));
`);

test("shared selection preserves the exact model ID for every backend", () => {
  const peer = { token: "team:user/%", name: "队友" };
  for (const kind of kinds) {
    for (const model of ["provider/model", "provider/model:high", "模型 / 100%", "gpt-5.6"]) {
      const props = { agentKind: kind, sharedModels: [{ peer, options: { [kind]: [{ value: model, name: model }] } }] };
      const [{ option, decoded }] = select(props, () => false, kinds);
      assert.deepEqual(decoded, { agentKind: kind, model, peerToken: peer.token });
      assert.equal(option.favoriteId, option.value);
      assert.equal(select(props, () => true, kinds)[0].option.value, model);
    }
  }
});
