import assert from "node:assert/strict";
import { collectWorkspaceArtifacts } from "../src/workspaceArtifacts.ts";

const items = [
  { type: "assistant", text: "[文件](<D:/项目/a b.md:12>) ![图](output.png) [网站](https://example.com)" },
  { type: "tool", content: [{ type: "diff", path: "output.png" }, { type: "diff", path: "src/main.ts" }] },
];
assert.deepEqual(collectWorkspaceArtifacts(items), ["output.png", "src/main.ts", "D:/项目/a b.md"]);
assert.deepEqual(collectWorkspaceArtifacts([{ type: "assistant", text: "[x](file:///D:/a%20b.md#L12)" }]), ["D:/a b.md"]);
assert.equal(collectWorkspaceArtifacts(Array.from({ length: 300 }, (_, i) => ({ type: "tool", content: [{ type: "diff", path: `${i}.txt` }] }))).length, 200);
console.log("workspace artifacts checks passed");
