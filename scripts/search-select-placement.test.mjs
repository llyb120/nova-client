import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import ts from "typescript";

// Execute the component's actual placement logic with viewport/element rectangles.
const source = readFileSync(new URL("../src/components/SearchSelect.tsx", import.meta.url), "utf8");
const placement = source.slice(source.indexOf("  const computePlacement ="), source.indexOf("  const toggle ="));
const { outputText } = ts.transpileModule(`${placement}\ncomputePlacement();`, {
  compilerOptions: { target: ts.ScriptTarget.ES2022 },
});
const run = new Function("rootRef", "props", "window", "popWidth", "isThreeLevel", "isGrouped", "usePortal", "setPlace", "setCoords", outputText);
const trigger = { top: 530, bottom: 556, left: 250, right: 460 };
const container = { top: 222, bottom: 581, left: 45, right: 925, width: 880 };
function position(rect, anchor, viewport = { innerWidth: 974, innerHeight: 592 }) {
  let coords;
  run(
    { getBoundingClientRect: () => rect, closest: () => ({ getBoundingClientRect: () => anchor }) },
    { anchorTo: ".composer", searchable: true, options: [] }, viewport,
    () => 640, () => true, () => false, () => true, () => {}, (value) => { coords = value; },
  );
  return coords;
}
const above = position(trigger, container);
assert.equal(592 - above.bottom, trigger.top - 8, "panel must stay adjacent to the model button");
assert.deepEqual(position(trigger, { ...container, top: 80 }), above, "growing the composer must not move the panel");
assert.ok(above.left >= container.left && above.left + above.width <= container.right);
const below = position({ ...trigger, top: 20, bottom: 46 }, container);
assert.equal(below.top, 54, "a trigger near the viewport top opens downward");
const narrow = position(trigger, container, { innerWidth: 500, innerHeight: 592 });
assert.ok(narrow.left >= 8 && narrow.left + narrow.width <= 492);
console.log("SearchSelect placement checks passed");
