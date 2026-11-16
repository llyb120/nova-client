import assert from "node:assert/strict";
import { build } from "esbuild";

const result = await build({
  stdin: {
    contents: `export { clipboardFiles, MAX_ATTACHMENT_BYTES } from './src/pasteAttachments';`,
    resolveDir: process.cwd(),
    loader: "ts",
  },
  bundle: true,
  write: false,
  format: "esm",
  platform: "neutral",
});
const { clipboardFiles, MAX_ATTACHMENT_BYTES } = await import(
  `data:text/javascript;base64,${Buffer.from(result.outputFiles[0].text).toString("base64")}`
);

const file = (name, type, size = 1024) => ({ name, type, size });
const item = value => ({ kind: value === null ? "string" : "file", getAsFile: () => value });

const image = file("a.png", "image/png");
const document = file("b.pdf", "application/pdf");
let pasted = clipboardFiles([item(image), item(document)]);
assert.deepEqual(pasted.files, [image, document], "Ctrl+V 的非图片文件同样成为附件");
assert.equal(pasted.oversized, 0);

const exact = file("edge.bin", "", MAX_ATTACHMENT_BYTES);
pasted = clipboardFiles([item(file("big.zip", "application/zip", MAX_ATTACHMENT_BYTES + 1)), item(exact)]);
assert.deepEqual(pasted.files, [exact], "上限内的文件保留");
assert.equal(pasted.oversized, 1, "超限文件只计数，不读进内存");

pasted = clipboardFiles([item(null), item(null)]);
assert.equal(pasted.files.length, 0);
assert.equal(pasted.oversized, 0, "纯文本粘贴和拿不到 File 的项不拦截");

// DataTransferItemList 是 ArrayLike：直接传，不必先展开成数组。
const list = { length: 2, 0: item(document), 1: item(image) };
assert.deepEqual(clipboardFiles(list).files, [document, image]);

console.log("Paste attachment checks passed (images and files, 20MB limit, non-file items ignored).");
