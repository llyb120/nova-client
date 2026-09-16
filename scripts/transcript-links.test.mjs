import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import ts from "typescript";
import { linkedFile } from "../src/workspaceLinks.ts";

// Run the actual component functions without mounting Solid or invoking native apps.
const source = readFileSync(new URL("../src/components/CanvasTranscript.tsx", import.meta.url), "utf8");
const ast = ts.createSourceFile("CanvasTranscript.tsx", source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
const names = new Set(["tokenizeInline", "linkedFile", "hitLink", "onClick", "onContextMenu"]);
const units = [];
function visit(node) {
  if (ts.isFunctionDeclaration(node) && names.has(node.name?.text)) units.push(node.getText(ast));
  ts.forEachChild(node, visit);
}
visit(ast);
const calls = [];
const context = vm.createContext({
  linkedFile,
  openInEditor: (...args) => calls.push(["workspace", ...args]),
  blocks: [{ textLines: [{ text: "go", x: 10, y: 20, lh: 20, charX: [0, 10, 20], charLinks: ["/tmp/a.ts:12", "https://example.com"] }] }],
  canvasEl: { getBoundingClientRect: () => ({ left: 100, top: 100 }) },
  scrollY: 0, hitTest: () => 0, hitBlockScrollbar: () => false, hitScrollbar: () => false,
  selMoved: false, selecting: false, props: { threadId: "thread" },
  api: Object.fromEntries(["openUrl", "openInEditor", "openFileDefault"].map(name => [name, (...args) => { calls.push([name, ...args]); return Promise.resolve(); }])),
  fileMenu: { open: (_e, path) => calls.push(["menu", path]) },
});
vm.runInContext(ts.transpileModule(units.join("\n"), { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText, context);
const run = code => vm.runInContext(code, context);
for (const [input, path, line] of [
  ["D:/project/a.ts:12", "D:/project/a.ts", 12],
  ["/D:/My%20Project/a.ts#L9", "D:/My Project/a.ts", 9],
  ["file:///tmp/a.ts:3", "/tmp/a.ts", 3],
  ["file:///C:/a.ts", "C:/a.ts", undefined],
  ["file://server/share/a.ts", "//server/share/a.ts", undefined],
  ["src/100%.ts", "src/100%.ts", undefined],
]) {
  const result = context.linkedFile(input);
  assert.equal(result.path, path);
  assert.equal(result.line, line);
}
for (const href of ["https://example.com", "javascript:alert(1)", "data:text/html,hi", "#section"]) assert.equal(context.linkedFile(href), null);
for (const [md, href] of [
  ["[file](</D:/My Project/a.ts:12>)", "/D:/My Project/a.ts:12"],
  ["[file](src/a(test).ts)", "src/a(test).ts"],
  ["[site](https://example.com)", "https://example.com"],
]) assert.equal(context.tokenizeInline(md)[0].link, href);
assert.equal(run("hitLink(111, 125)"), "/tmp/a.ts:12");
assert.equal(run("hitLink(121, 125)"), "https://example.com");
assert.equal(run("hitLink(131, 125)"), undefined);
assert.equal(run("hitLink(111, 145)"), undefined);
run("onClick({clientX:111, clientY:125})");
run("onClick({clientX:121, clientY:125})");
run("onContextMenu({clientX:111, clientY:125, preventDefault(){}})");
run("selMoved = true; onClick({clientX:111, clientY:125})");
assert.deepEqual(calls, [["workspace", "/tmp/a.ts", 12], ["openUrl", "https://example.com"], ["menu", "/tmp/a.ts"]]);
console.log("Transcript links: parsing, hit testing, click dispatch, context menu and drag selection passed.");
