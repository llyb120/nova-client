import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";

const { outputFiles } = await build({
  entryPoints: [fileURLToPath(new URL("../src/meteorSelection.ts", import.meta.url))],
  bundle: true,
  write: false,
  platform: "node",
  format: "esm",
});
const { meteorRegion, meteorRequest, meteorMonitor } = await import(`data:text/javascript;base64,${Buffer.from(outputFiles[0].text).toString("base64")}`);
const bounds = { left: 100, top: 50, width: 800, height: 600 };
const image = { width: 1600, height: 1200 };
const region = (start, end) => meteorRegion(start, end, bounds, image);

assert.deepEqual(region({ x: 300, y: 200 }, { x: 302, y: 202 }), { x: 400, y: 300, width: 0, height: 0 }, "small pointer jitter remains a point, converted to screenshot pixels");
const selection = region({ x: 700, y: 500 }, { x: 300, y: 200 });
assert.deepEqual(selection, { x: 400, y: 300, width: 800, height: 600 }, "reverse drags are normalized at 200% DPI");
assert.deepEqual(region({ x: -50, y: -50 }, { x: 1500, y: 1500 }), { x: 0, y: 0, width: 1600, height: 1200 }, "captured pointer outside the screen is clamped");
assert.deepEqual(region({ x: 900, y: 650 }, { x: 900, y: 650 }), { x: 1599, y: 1199, width: 0, height: 0 }, "points stay within screenshot bounds");
assert.deepEqual(region({ x: 900, y: 650 }, { x: 900, y: 640 }), { x: 1599, y: 1180, width: 1, height: 20 }, "thin selections on the image edge never exceed its bounds");
assert.deepEqual(meteorRegion({ x: 1, y: 1 }, { x: 10, y: 10 }, { left: 0, top: 0, width: 100, height: 100 }, { width: 150, height: 125 }), { x: 1, y: 1, width: 14, height: 12 }, "fractional and independent axis scaling cover the selected pixels");
assert.equal(region({ x: NaN, y: 1 }, { x: 2, y: 3 }), null);
assert.equal(meteorRegion({ x: 1, y: 1 }, { x: 2, y: 3 }, { ...bounds, width: 0 }, image), null);
const note = (text, selected = selection) => ({ text, region: selected });
assert.equal(meteorRequest("capture", [note(" \n\t ")]), null, "all-blank requests cannot be submitted");
assert.equal(meteorRequest("capture", []), null, "at least one annotation is required");
assert.equal(meteorRequest("", [note("操作")]), null, "a capture ID is required");
const annotations = [note("  请检查这份表格 👀\n"), note(" ", { x: 100, y: 200, width: 0, height: 0 })];
assert.deepEqual(meteorRequest("capture", annotations), { captureId: "capture", annotations: [note("请检查这份表格 👀"), note("", annotations[1].region)] });
assert.equal(annotations[0].text, "  请检查这份表格 👀\n", "normalizing cannot modify editable annotations");
assert.ok(meteorRequest("capture", Array.from({ length: 20 }, () => note("操作"))));
assert.equal(meteorRequest("capture", Array.from({ length: 21 }, () => note("操作"))), null);
assert.ok(meteorRequest("capture", [note("👀".repeat(16000))]), "Unicode code points, not UTF-16 units");
assert.equal(meteorRequest("capture", [note("a".repeat(16000)), note("b")]), null, "length limit spans all annotations");
assert.equal(meteorRequest("capture", [note("操作", { x: 0, y: 0, width: 1, height: 0 })]), null);
const left = { x: 0, y: 0, width: 1920, height: 1080 };
const right = { x: 1920, y: 180, width: 2560, height: 1440 };
// Physical desktop origin may have been negative; the backend normalizes both displays to image pixels.
const dual = { width: 4480, height: 1620, monitors: [left, right], pointer: { x: 2300, y: 600 } };
assert.deepEqual(meteorMonitor(dual), right, "toolbar follows the trigger pointer's actual display");
assert.deepEqual(meteorMonitor(dual, { x: 100, y: 100, width: 0, height: 0 }), left);
assert.deepEqual(meteorMonitor(dual, { x: 1800, y: 200, width: 1000, height: 500 }), right, "cross-display selection picks its greatest overlap");
assert.deepEqual(meteorMonitor(dual, { x: 2300, y: 10, width: 0, height: 0 }), right, "a point in the virtual desktop gap chooses the nearest real display");
const upper = { x: 0, y: 0, width: 1600, height: 900 };
const lower = { x: 320, y: 900, width: 1600, height: 1200 };
const vertical = { width: 1920, height: 2100, monitors: [upper, lower], pointer: { x: 600, y: 1400 } };
assert.deepEqual(meteorMonitor(vertical), lower);
assert.deepEqual(meteorMonitor(vertical, { x: 700, y: 800, width: 400, height: 150 }), upper);
assert.deepEqual(meteorMonitor(image), { x: 0, y: 0, ...image }, "legacy contexts remain single-display");
console.log("Meteor overlay: multi-annotation validation, Unicode limits, DPI mapping and horizontal/vertical monitor selection passed.");
