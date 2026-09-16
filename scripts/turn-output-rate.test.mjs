// Run: node scripts/turn-output-rate.test.mjs
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import ts from "typescript";

const source = readFileSync(new URL("../src/components/TurnGroup.tsx", import.meta.url), "utf8");
const file = ts.createSourceFile("TurnGroup.tsx", source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
const fn = file.statements.find((node) => ts.isFunctionDeclaration(node) && node.name?.text === "turnAvgTokensPerSec");
assert.ok(fn);
const context = vm.createContext({});
vm.runInContext(ts.transpile(fn.getText(file).replace(/^export /, "")), context);
const rate = context.turnAvgTokensPerSec;
assert.equal(rate({ totalTokens: 1_135_900, outputTokens: 6140, durationMs: 307_000 }), 20);
assert.equal(rate({ totalTokens: 1_135_900, durationMs: 307_000 }), null);
assert.equal(rate({ outputTokens: 0, durationMs: 1000 }), 0);
assert.equal(rate({ outputTokens: 100, durationMs: 999 }), null);
assert.equal(rate({ outputTokens: -1, durationMs: 1000 }), null);
assert.equal(rate({ outputTokens: Infinity, durationMs: 1000 }), null);
assert.equal(rate({ outputTokens: 100, durationMs: NaN }), null);
assert.equal(rate(null), null);
console.log("turn output rate checks passed");
