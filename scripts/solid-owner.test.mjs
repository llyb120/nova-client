import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { test } from "node:test";
import { build } from "esbuild";
import { transform } from "@solidjs/compiler";
import { chromium } from "playwright-core";

const sidebar = readFileSync(new URL("../src/components/Sidebar.tsx", import.meta.url), "utf8");
const vite = readFileSync(new URL("../vite.config.ts", import.meta.url), "utf8");

test("dynamic list callbacks do not create unowned Solid computations", () => {
  assert.doesNotMatch(sidebar, /\{\(row\)\s*=>\s*ThreadRow\(/);
  assert.doesNotMatch(sidebar, /const rows = createMemo\(/);
});

test("Solid dev HMR is disabled with the app's disabled Vite HMR", () => {
  assert.match(vite, /solid\(\{\s*refresh:\s*false\s*\}\)/);
});

test("Solid errors show a modal with the original error in development and production", async () => {
  const entry = readFileSync(new URL("../src/index.tsx", import.meta.url), "utf8");
  const render = entry.slice(entry.lastIndexOf("  render("), entry.lastIndexOf("\n}"));
  assert.match(render, /<Errored fallback=.*<SolidErrorDialog error=\{error\}/);
  const fixture = `
    import { createEffect, createMemo, createSignal, Errored } from 'solid-js';
    import { render } from '@solidjs/web';
    import { SolidErrorDialog } from './src/components/SolidErrorDialog';
    const [value, setValue] = createSignal(0);
    window.trigger = () => setValue(1);
    function App() {
      if (window.mode === 'mount') throw new Error('solid mount failure');
      if (window.mode === 'string') throw 'plain <img src=x>';
      const text = createMemo(() => {
        if (window.mode === 'memo' && value()) throw new Error('solid memo failure');
        return 'ready';
      });
      createEffect(() => value(), n => {
        if (window.mode === 'effect' && n) throw new Error('solid effect failure');
      });
      return <p>{text()}</p>;
    }
    ${render}
  `;
  const executablePath = [process.env.TEST_BROWSER, chromium.executablePath(),
    `${process.env.ProgramFiles}/Google/Chrome/Application/chrome.exe`,
    `${process.env['ProgramFiles(x86)']}/Microsoft/Edge/Application/msedge.exe`, '/usr/bin/chromium']
    .find(path => path && existsSync(path));
  assert.ok(executablePath, 'Set TEST_BROWSER to an installed Chromium');
  const browser = await chromium.launch({ headless: true, executablePath });
  try {
    for (const dev of [false, true]) {
      const compiled = await build({
        stdin: { contents: transform(fixture, { filename: 'fixture.tsx', dev }).code, resolveDir: process.cwd(), loader: 'ts' },
        bundle: true, write: false, platform: 'browser', format: 'esm', conditions: ['browser', ...(dev ? ['development'] : [])],
        plugins: [{ name: 'solid-fixture', setup(builder) {
          builder.onLoad({ filter: /\.tsx$/ }, ({ path }) => ({ loader: 'ts', contents:
            transform(readFileSync(path, 'utf8'), { filename: path, dev }).code }));
        } }],
      });
      for (const mode of ['mount', 'memo', 'effect', 'string']) {
        const page = await browser.newPage();
        const errors = [];
        page.on('pageerror', error => errors.push(error.message));
        await page.setContent('<div id="root"></div>');
        await page.evaluate(mode => { window.mode = mode; }, mode);
        await page.addScriptTag({ type: 'module', content: compiled.outputFiles[0].text });
        if (mode === 'memo' || mode === 'effect') {
          await page.locator('p').filter({ hasText: 'ready' }).waitFor();
          assert.equal(await page.locator('dialog').count(), 0);
          await page.evaluate(() => window.trigger());
        }
        const dialog = page.locator('dialog[open]');
        await dialog.waitFor();
        const detail = await dialog.locator('pre').innerText();
        if (mode === 'string') assert.equal(detail, 'plain <img src=x>');
        else {
          assert.match(detail, new RegExp(`Error: solid ${mode} failure`));
          assert.match(detail, /\n\s+at /, 'the stack must remain visible');
        }
        assert.equal(await dialog.locator('img').count(), 0, 'errors are text, not HTML');
        assert.ok(await dialog.evaluate(el => el.matches(':modal') && el.contains(document.activeElement)));
        await page.keyboard.press('Escape');
        assert.ok(await dialog.isVisible(), 'dismissal must not leave an empty window');
        assert.deepEqual(errors, []);
        await page.close();
      }
    }
  } finally { await browser.close(); }
});
