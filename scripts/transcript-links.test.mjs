import assert from "node:assert/strict";
import { linkedFile } from "../src/workspaceLinks.ts";

for (const [input, path, line] of [
  ["D:/project/a.ts:12", "D:/project/a.ts", 12],
  ["/D:/My%20Project/a.ts#L9", "D:/My Project/a.ts", 9],
  ["file:///tmp/a.ts:3", "/tmp/a.ts", 3],
  ["file:///C:/a.ts", "C:/a.ts", undefined],
  ["file://localhost/tmp/a.ts", "/tmp/a.ts", undefined],
  ["file://server/share/a.ts", "//server/share/a.ts", undefined],
  ["src/100%.ts", "src/100%.ts", undefined],
  ["C:\\project\\a.ts:12", "C:\\project\\a.ts", 12],
  // A bare `name.ext:line` looks like a URI scheme but is a file reference.
  ["a.ts:12", "a.ts", 12],
  ["a.ts:12:4", "a.ts", 12],
  ["src/a.ts#L12C4-L20C8", "src/a.ts", 12],
  // Encoded `#`/`:` belong to the filename, not to a source location.
  ["file:///C:/report%23L12", "C:/report#L12", undefined],
  ["src/name%3A12", "src/name:12", undefined],
]) {
  assert.deepEqual(linkedFile(input), { path, line }, input);
}
for (const href of ["", "https://example.com", "https://example.com:123", "mailto:a@b.c", "javascript:alert(1)", "javascript:123", "data:text/html,hi", "#section"]) {
  assert.equal(linkedFile(href), null, href);
}
console.log("Transcript file links: paths, URI escaping, source locations and external schemes passed.");
