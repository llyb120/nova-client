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
const { browserLaunchOptions, contextLaunchOptions } = require(recorder);

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
