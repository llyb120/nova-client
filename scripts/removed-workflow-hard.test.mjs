// Run: node scripts/removed-workflow-hard.test.mjs (no external dependencies).
import assert from "node:assert/strict";
import { existsSync, readFileSync, readdirSync } from "node:fs";

const root = new URL("../", import.meta.url);
const read = (path) => readFileSync(new URL(path, root), "utf8");
for (const path of ["src/workflow", "src/hard"]) {
  const dir = new URL(path, root);
  if (existsSync(dir)) assert.deepEqual(readdirSync(dir), [], `${path}: removed modules remain`);
}
for (const path of ["src/hard.ts", ...["WorkflowCanvas", "WorkflowPicker", "WorkflowPreview", "WorkflowSettingsPanel", "WorkflowsView"].map((name) => `src/components/${name}.tsx`)]) {
  assert.equal(existsSync(new URL(path, root)), false, path);
}
const removedEntry = /\b\w*Workflow\w*|\bworkflow[A-Z_]\w*|\bworkflows\b|\/workflow\/|\b\w*HardDesign\w*|\bparseRunInput\b|\/hard\b|["']hard["']/;
// Include shared helpers (not just UI entry points); never descend into assets or build/dependency trees.
const dirs = ["src"];
for (const dir of dirs) {
  for (const entry of readdirSync(new URL(dir, root), { withFileTypes: true })) {
    const path = `${dir}/${entry.name}`;
    if (entry.isDirectory() && !["assets", "node_modules", "dist", "build", "target", ".git", "generated", "__generated__"].includes(entry.name)) dirs.push(path);
    if (entry.isFile() && /\.tsx?$/.test(entry.name)) {
      assert.ok(!removedEntry.test(read(path)), `${path}: removed feature entry remains`);
    }
  }
}
const slash = read("src/components/slashSuggestions.ts");
assert.doesNotMatch(slash, /name:\s*["'](?:run|hard)["']/);
for (const command of ["stage", "fire", "easy", "plan", "setup"]) {
  assert.match(slash, new RegExp(`name:\\s*["']${command}["']`), command);
}
assert.ok(/export const UNIFIED_MODES[^;]*id:\s*"build"/.test(read("src/store.ts")), "Build mode remains available");
assert.match(read("src/builtinPrompts.ts"), /export function buildEasyPrompt\(/);
assert.match(read("src/builtinPrompts.ts"), /export function buildPlanPrompt\(/);
const guide = read("使用文档.html");
assert.doesNotMatch(guide, /v-workflow|工作流|\/(?:run|hard)\b/);
assert.match(guide, /id="v-normal">普通模式/);
console.log("Removed workflow/Hard modules and entries; Build and ordinary commands preserved: OK");
