import assert from "node:assert/strict";
import { readFileSync, existsSync } from "node:fs";
import { transform } from "esbuild";
import { chromium } from "playwright-core";

const { code } = await transform(readFileSync(new URL("../src/components/signaturePaths.ts", import.meta.url), "utf8"), { loader: "ts", format: "esm" });
const { signaturePaths, signatureFrame } = await import(`data:text/javascript;base64,${Buffer.from(code).toString("base64")}`);
const identities = [...readFileSync(new URL("../src-tauri/src/signature.rs", import.meta.url), "utf8").matchAll(/\("([a-z.]+)", "\d{3}"\)/g)].map((m) => m[1]);
assert.deepEqual(Object.keys(signaturePaths).sort(), identities.sort(), "every identity needs its own drawing");
assert.equal(new Set(Object.values(signaturePaths).map((d) => d.strokes.join())).size, identities.length);
assert.deepEqual(signatureFrame([20, 30], 0).map((s) => s.drawn), [0, 0]);
assert.deepEqual(signatureFrame([20, 30], 1).map((s) => s.drawn), [20, 30]);
assert.deepEqual(signatureFrame([20, 30], -1).map((s) => s.drawn), [0, 0]);
assert.deepEqual(signatureFrame([20, 30], 2).map((s) => s.drawn), [20, 30]);
assert(signatureFrame([20, 30], 25 / 60).every((s) => !s.active), "pen lifts between strokes");
const edge = "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe";
const browser = await chromium.launch({ headless: true, ...(existsSync(edge) ? { executablePath: edge } : {}) });
try {
  const page = await browser.newPage();
  for (const [name, drawing] of Object.entries(signaturePaths)) {
    for (const stroke of drawing.strokes) assert.equal((stroke.match(/M/gi) ?? []).length, 1, `${name}: no invisible jump inside a stroke`);
    const lengths = await page.evaluate(({ width, strokes }) => {
      document.body.innerHTML = `<svg viewBox="0 0 ${width} 94">${strokes.map((d) => `<path d="${d}" fill="none" stroke="black"/>`).join("")}</svg>`;
      return [...document.querySelectorAll("path")].map((path) => {
        const b = path.getBBox();
        if (b.x < 0 || b.y < 0 || b.x + b.width > width || b.y + b.height > 94) throw new Error("clipped stroke");
        return path.getTotalLength();
      });
    }, drawing);
    assert(lengths.every((n) => Number.isFinite(n) && n > 0), name);
    let previous = lengths.map(() => 0);
    for (let step = 0; step <= 100; step++) {
      const frame = signatureFrame(lengths, step / 100);
      assert(frame.filter((s) => s.active).length <= 1, `${name}: only one pen`);
      frame.forEach((s, i) => {
        assert(s.drawn >= previous[i] && s.drawn <= lengths[i]);
        if (s.active) assert(frame.slice(0, i).every((p, j) => p.drawn === lengths[j]), `${name}: stroke order`);
      });
      previous = frame.map((s) => s.drawn);
    }
    assert.deepEqual(previous, lengths, `${name}: complete final signature`);
  }
  console.log(`Signature checks passed: ${identities.length} custom drawings, browser SVG geometry and stroke order.`);
} finally {
  await browser.close();
}
