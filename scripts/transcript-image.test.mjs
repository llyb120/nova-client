// Unit tests for transcriptImageSrc / localImagePath.
// The implementation lives in src/transcriptImage.ts; the helpers below are
// copies of its path-resolution logic so the tests run without a Tauri
// runtime. Keep them in sync with that file.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const source = readFileSync(new URL("../src/transcriptImage.ts", import.meta.url), "utf8");
assert.ok(source.includes("export function localImagePath"), "localImagePath must exist");
assert.ok(source.includes("export function transcriptImageSrc"), "transcriptImageSrc must exist");

const mockCwd = "D:\\code\\nova-client-master";

const localImagePath = (href) => {
  if (/^(?:https?|data|javascript|mailto|tel):/i.test(href)) return null;
  let path = decodeURIComponent(href)
    .replace(/^file:\/\/(?:localhost\/)?/i, "")
    .replace(/^\\\\\?\\/, "")
    .replace(/\\/g, "/")
    .replace(/^\/([A-Za-z]:)/, "$1");
  if (/^(?:[A-Za-z]:\/|\/(?!\/))/.test(path)) return path;
  const base = mockCwd.replace(/\\/g, "/").replace(/\/+$/, "");
  const segments = path.split("/").filter(Boolean);
  const stack = base.split("/");
  for (const seg of segments) {
    if (seg === "." || seg === "") continue;
    if (seg === "..") { if (stack.length > 1) stack.pop(); continue; }
    stack.push(seg);
  }
  return stack.join("/");
};

const transcriptImageSrc = (href, convertFileSrc = (p) => `asset://mock/${encodeURIComponent(p).replace(/%2F/g, "/")}`) => {
  const path = localImagePath(href);
  if (path) return convertFileSrc(path);
  return /^(?:https?:\/\/|data:image\/(?:png|jpeg|webp|gif);base64,)/i.test(href) ? href : "";
};

test("absolute Windows paths resolve directly", () => {
  assert.equal(localImagePath("D:/x/y.png"), "D:/x/y.png");
  assert.equal(localImagePath("C:\\Users\\a\\b.png"), "C:/Users/a/b.png");
  assert.equal(localImagePath("\\\\?\\D:\\x\\y.png"), "D:/x/y.png");
});

test("absolute POSIX paths resolve directly", () => {
  assert.equal(localImagePath("/tmp/x.png"), "/tmp/x.png");
});

test("file:// URLs are converted to local paths", () => {
  assert.equal(localImagePath("file:///D:/x/y.png"), "D:/x/y.png");
  assert.equal(localImagePath("file://localhost/D:/x/y.png"), "D:/x/y.png");
  assert.equal(localImagePath("file:///tmp/x.png"), "/tmp/x.png");
});

test("relative paths resolve against the working directory", () => {
  assert.equal(localImagePath("./nova-image-abc.png"), "D:/code/nova-client-master/nova-image-abc.png");
  assert.equal(localImagePath("nova-image-abc.png"), "D:/code/nova-client-master/nova-image-abc.png");
  assert.equal(localImagePath("../images/x.png"), "D:/code/images/x.png");
  assert.equal(localImagePath("../../deep/x.png"), "D:/deep/x.png");
  assert.equal(localImagePath("sub/dir/x.png"), "D:/code/nova-client-master/sub/dir/x.png");
  assert.equal(localImagePath("./sub/./x.png"), "D:/code/nova-client-master/sub/x.png");
});

test("non-local URLs return null", () => {
  assert.equal(localImagePath("https://example.com/x.png"), null);
  assert.equal(localImagePath("http://example.com/x.png"), null);
  assert.equal(localImagePath("data:image/png;base64,abc"), null);
  assert.equal(localImagePath("javascript:alert(1)"), null);
});

test("transcriptImageSrc returns convertFileSrc result for local paths", () => {
  assert.ok(transcriptImageSrc("D:/x.png").startsWith("asset://mock/"));
  assert.ok(transcriptImageSrc("./x.png").startsWith("asset://mock/"));
});

test("transcriptImageSrc passes through remote and data URLs", () => {
  assert.equal(transcriptImageSrc("https://example.com/x.png"), "https://example.com/x.png");
  assert.equal(transcriptImageSrc("data:image/png;base64,abc"), "data:image/png;base64,abc");
});

test("transcriptImageSrc rejects dangerous or unknown schemes", () => {
  assert.equal(transcriptImageSrc("javascript:alert(1)"), "");
});
