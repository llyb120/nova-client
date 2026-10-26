import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { transformSync } from "esbuild";

const source = readFileSync(new URL("../src/transcriptHeights.ts", import.meta.url), "utf8");
const { code } = transformSync(source, { loader: "ts", format: "esm", target: "es2022" });
const { TranscriptHeights } = await import(`data:text/javascript;base64,${Buffer.from(code).toString("base64")}`);

function check(actual, expected) {
  assert.equal(actual.length, expected.length);
  let sum = 0;
  for (let i = 0; i < expected.length; i++) {
    assert.equal(actual.get(i), expected[i]);
    assert.equal(actual.offset(i), sum);
    assert.equal(actual.at(sum), i);
    assert.equal(actual.at(sum + expected[i] / 2), i);
    sum += expected[i];
  }
  assert.equal(actual.offset(expected.length), sum);
  assert.equal(actual.total, sum);
  assert.equal(actual.at(sum), Math.max(0, expected.length - 1));
  assert.equal(actual.at(Infinity), Math.max(0, expected.length - 1));
}

function randomSource() {
  let seed = 0x12345678;
  return () => ((seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0) / 2 ** 32);
}

test("defaults, half-open boundaries, clamping and non-finite inputs", () => {
  const heights = new TranscriptHeights();
  check(heights, []);
  assert.equal(heights.get(0), 0);
  assert.equal(heights.set(0, 20), false);
  assert.equal(heights.offset(Infinity), 0);
  heights.resize(3);
  check(heights, [240, 240, 240]);
  for (const [y, index] of [[-10, 0], [0, 0], [24, 0], [239.9, 0], [240, 1], [480, 2], [1e6, 2], [NaN, 0], [-Infinity, 0]]) {
    assert.equal(heights.at(y), index);
  }
  for (const index of [-1, 0.5, 3, NaN, Infinity, -Infinity]) {
    assert.equal(heights.get(index), 0);
    assert.equal(heights.set(index, 50), false);
  }
  for (const height of [NaN, Infinity, -Infinity]) assert.equal(heights.set(1, height), false);
  assert.equal(heights.set(1, 0), true);
  assert.equal(heights.set(1, -5), false);
  assert.equal(heights.set(1, 0.5), false);
  assert.equal(heights.set(1, 13.75), true);
  assert.equal(heights.set(1, 13.75), false);
  check(heights, [240, 13.75, 240]);
  for (const [index, offset] of [[NaN, 0], [-Infinity, 0], [-1, 0], [1.9, 240], [Infinity, 493.75]]) {
    assert.equal(heights.offset(index), offset);
  }
  for (const count of [-1, 1.5, NaN, Infinity, -Infinity, 2 ** 31]) {
    assert.throws(() => new TranscriptHeights(count), RangeError);
    assert.throws(() => heights.resize(count), RangeError);
    assert.throws(() => heights.reset(count), RangeError);
  }
  check(heights, [240, 13.75, 240]); // Rejected counts must not discard measurements.
  for (const estimate of [0, -20, 0.5, NaN, Infinity, -Infinity]) {
    check(new TranscriptHeights(2, estimate), Array(2).fill(Number.isFinite(estimate) ? 1 : 240));
  }
});

test("randomized set/resize/reset and y lookups match naive heights", () => {
  const random = randomSource();
  const estimate = 37.25;
  const heights = new TranscriptHeights(64, estimate);
  let expected = Array(64).fill(estimate);
  for (let operation = 0; operation < 4000; operation++) {
    const kind = Math.floor(random() * 10);
    if (kind < 6) {
      const index = Math.floor(random() * (expected.length + 4)) - 2;
      const height = Math.floor(random() * 4000) / 4;
      const normalized = Math.max(1, height);
      const valid = index >= 0 && index < expected.length;
      assert.equal(heights.set(index, height), valid && expected[index] !== normalized);
      if (valid) expected[index] = normalized;
    } else {
      const count = Math.floor(random() * 129);
      if (kind === 9) {
        heights.reset(count);
        expected = Array(count).fill(estimate);
      } else {
        heights.resize(count);
        expected = expected.slice(0, count);
        while (expected.length < count) expected.push(estimate);
      }
    }
    check(heights, expected);
    const y = random() * (heights.total + 200) - 100;
    let prefix = 0;
    const index = expected.findIndex(height => y < (prefix += height));
    assert.equal(heights.at(y), index < 0 ? Math.max(0, expected.length - 1) : index);
  }
});

test("width invalidation resets measurements, including at unchanged count", () => {
  const heights = new TranscriptHeights(9, 80.5);
  heights.set(0, 15);
  heights.set(4, 500);
  heights.set(8, 42);
  heights.resize(9);
  assert.equal(heights.get(4), 500);
  heights.resize(5);
  heights.resize(9);
  check(heights, [15, 80.5, 80.5, 80.5, 500, 80.5, 80.5, 80.5, 80.5]);
  heights.reset(9);
  check(heights, Array(9).fill(80.5));
  heights.set(0, 999);
  heights.reset(17);
  check(heights, Array(17).fill(80.5));
  heights.reset(0);
  heights.resize(3);
  check(heights, Array(3).fill(80.5));
});

test("100k groups, repeated token updates and tail growth across powers of two", () => {
  const heights = new TranscriptHeights();
  const expected = [];
  let sum = 0;
  for (let i = 0; i < 100000; i++) {
    heights.resize(i + 1);
    const height = 1 + (i % 97) / 4;
    assert.equal(heights.set(i, height), true);
    expected.push(height);
    sum += height;
    if (i % 4096 === 0) assert.equal(heights.total, sum);
  }
  const random = randomSource();
  for (let i = 0; i < 10000; i++) {
    heights.resize(100000); // Per-token no-op must not rebuild history.
    const index = i % 2 ? 99999 : Math.floor(random() * expected.length);
    const height = 1 + Math.floor(random() * 4000) / 4;
    assert.equal(heights.set(index, height), expected[index] !== height);
    expected[index] = height;
  }
  check(heights, expected);
  for (let count = 100001; count <= 131080; count++) {
    heights.resize(count);
    expected.push(240);
    assert.equal(heights.at(heights.total - 0.5), count - 1);
  }
  check(heights, expected);
  heights.resize(65535);
  expected.length = 65535;
  heights.resize(65537);
  expected.push(240, 240);
  check(heights, expected);
});
