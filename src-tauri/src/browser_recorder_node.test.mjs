import assert from "node:assert/strict";
import test from "node:test";
import { createRequire } from "node:module";
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const runtime = mkdtempSync(join(tmpdir(), "nova-browser-recorder-test-"));
const playwright = join(runtime, "node_modules", "playwright-core");
mkdirSync(playwright, { recursive: true });
writeFileSync(join(playwright, "index.js"), "exports.chromium = {};\n");
writeFileSync(join(runtime, "browser_collector.js"), "");
process.env.NODE_PATH = join(runtime, "node_modules");
process.env.NOVA_BROWSER_RECORDER_TEST = "1";

const require = createRequire(import.meta.url);
const source = readFileSync(new URL("./browser_recorder_node.js", import.meta.url), "utf8");
const recorder = join(runtime, "browser_recorder_node.cjs");
writeFileSync(recorder, source);
const {
  browserLaunchOptions,
  contextLaunchOptions,
  execShotPath,
  mapViewportPoint,
  normalizeDevUrl,
  pushDebugEvent,
  safeShotName,
  shouldExitAfterLastPageCloses,
  shouldRecoverLateSessionAuth,
} = require(recorder);

test("development URLs use https except for local servers", () => {
  assert.equal(normalizeDevUrl("example.com/app"), "https://example.com/app");
  assert.equal(normalizeDevUrl("localhost:5173"), "http://localhost:5173");
  assert.equal(normalizeDevUrl("127.0.0.1:3000"), "http://127.0.0.1:3000");
  assert.equal(normalizeDevUrl("http://dev.local"), "http://dev.local");
});

test("debug event buffer keeps the latest 200 entries", () => {
  const events = [];
  for (let index = 0; index < 205; index += 1) pushDebugEvent(events, { index });
  assert.equal(events.length, 200);
  assert.equal(events[0].index, 5);
  assert.equal(events[199].index, 204);
});

test("late session auth only retries a navigation that already received 401", () => {
  const startedAt = 1000;
  const unauthorized = [{ ts: 1001, kind: "response", status: 401 }];
  assert.equal(shouldRecoverLateSessionAuth(unauthorized, startedAt, "[]", '[["authorization","token"]]', false), true);
  assert.equal(shouldRecoverLateSessionAuth(unauthorized, startedAt, "same", "same", false), false);
  assert.equal(shouldRecoverLateSessionAuth([{ ts: 999, kind: "response", status: 401 }], startedAt, "[]", "token", false), false);
  assert.equal(shouldRecoverLateSessionAuth([{ ts: 1001, kind: "response", status: 403 }], startedAt, "[]", "token", false), false);
  assert.equal(shouldRecoverLateSessionAuth(unauthorized, startedAt, "[]", "token", true), false);
});

test("headless mode reaches Playwright launch and uses a deterministic viewport", () => {
  assert.deepEqual(browserLaunchOptions(true), { headless: true, timeout: 20000 });
  assert.deepEqual(contextLaunchOptions(true), { viewport: { width: 1280, height: 800 } });
});

test("visible mode keeps the system browser maximized", () => {
  assert.deepEqual(browserLaunchOptions(false), {
    headless: false,
    timeout: 20000,
    args: ["--start-maximized"],
  });
  assert.deepEqual(contextLaunchOptions(false), { viewport: null });
});

test("switching headless mode does not exit with the deliberately closed browser", () => {
  assert.equal(shouldExitAfterLastPageCloses(0, false, false), true);
  assert.equal(shouldExitAfterLastPageCloses(0, false, true), false);
  assert.equal(shouldExitAfterLastPageCloses(1, false, false), false);
});

test("normalized image points map to the actual page viewport", () => {
  assert.deepEqual(
    mapViewportPoint({ x: 250, y: 750, normalized: true }, { width: 1440, height: 900 }),
    { x: 360, y: 675 },
  );
  assert.deepEqual(
    mapViewportPoint({ x: 640, y: 400, imageWidth: 1280, imageHeight: 800 }, { width: 1440, height: 900 }),
    { x: 720, y: 450 },
  );
  assert.throws(
    () => mapViewportPoint({ x: 1001, y: 500, normalized: true }, { width: 1440, height: 900 }),
    /0\.\.1000/,
  );
});

test("unconfigured screenshots preserve their requested path", () => {
  assert.equal(execShotPath({ runId: "run-1", path: "C:/Temp/current.png" }), "C:/Temp/current.png");
});

test("plan screenshot names cannot escape the configured run directory", () => {
  assert.equal(safeShotName("../../结果.png"), "--.png");
  assert.equal(safeShotName(""), "current.png");
});
