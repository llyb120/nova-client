import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import ts from "typescript";

const source = readFileSync(new URL("../src/components/FileContextMenu.tsx", import.meta.url), "utf8");
const ast = ts.createSourceFile("menu.tsx", source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
const fn = ast.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === "copyImage");
const png = new Blob(["pixels"], { type: "image/png" });
let drawn = false;
let decoded = false;
let failDecode = false;
const canvas = {
  getContext: () => ({ drawImage: () => { drawn = true; } }),
  toBlob: (callback, type) => { assert.equal(type, "image/png"); callback(png); },
};
const context = vm.createContext({
  Image: class {
    naturalWidth = 800;
    naturalHeight = 600;
    async decode() { await Promise.resolve(); if (failDecode) throw new Error("decode failed"); decoded = true; }
  },
  convertFileSrc: path => { assert.equal(path, "D:/生成图片.png"); return "asset://image"; },
  document: { createElement: tag => { assert.equal(tag, "canvas"); return canvas; } },
  ClipboardItem: class { constructor(data) { this.data = data; } },
  navigator: { clipboard: { async write(items) {
    assert.equal(decoded, false, "clipboard write starts before async decode finishes");
    assert.equal(await items[0].data["image/png"], png);
  } } },
});
vm.runInContext(ts.transpileModule(fn.getText(ast), { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText, context);
await context.copyImage("D:/生成图片.png");
assert.equal(drawn, true);
assert.equal(canvas.width, 800);
assert.equal(canvas.height, 600);
decoded = false;
failDecode = true;
await assert.rejects(context.copyImage("D:/生成图片.png"), /decode failed/);
console.log("Copy image: PNG clipboard payload, original dimensions, user gesture and decode errors passed.");
