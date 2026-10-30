// Real Solid 2 DOM/lifecycle, isolated browser; no native window operations.
import assert from 'node:assert/strict';
import { access, readFile } from 'node:fs/promises';
import { build } from 'esbuild';
import { transform } from '@solidjs/compiler';
import { chromium } from 'playwright-core';

const fixture = `
  import { createSignal, Show } from 'solid-js';
  import { render } from '@solidjs/web';
  import { TitleBar, TitleBarPortal } from './src/components/TitleBar';
  const [shown, setShown] = createSignal(true);
  const [name, setName] = createSignal('first header');
  window.check = { setShown, setName };
  render(() => <>
    {!window.inline && <TitleBar />}
    <main><Show when={shown()}><TitleBarPortal>
      <header class="page-header"><input value={name()} /><span>{name()}</span></header>
    </TitleBarPortal></Show></main>
  </>, document.getElementById('root'));
`;
const compiled = await build({
  stdin: { contents: transform(fixture, { filename: 'fixture.tsx', dev: true }).code, resolveDir: process.cwd(), loader: 'ts' },
  bundle: true, write: false, platform: 'browser', format: 'esm', conditions: ['browser', 'development'],
  plugins: [{ name: 'native-window-fixture', setup(builder) {
    builder.onResolve({ filter: /^@tauri-apps\/api\/window$/ }, () => ({ path: 'window', namespace: 'fixture' }));
    builder.onResolve({ filter: /^\.\.\/sidebarLayout$/ }, () => ({ path: 'sidebar', namespace: 'fixture' }));
    builder.onLoad({ filter: /.*/, namespace: 'fixture' }, ({ path }) => ({ loader: 'js', contents: path === 'sidebar'
      ? 'export const sidebarLayout = { collapsed: false };'
      : 'export const getCurrentWindow = () => ({ isMaximized: async () => false, onResized: async () => () => {} });' }));
    builder.onLoad({ filter: /\.tsx$/ }, async ({ path }) => ({ loader: 'ts', contents:
      transform(await readFile(path, 'utf8'), { filename: path, dev: true }).code }));
  } }],
});
let executablePath;
for (const candidate of [process.env.TEST_BROWSER, chromium.executablePath(),
  `${process.env['ProgramFiles(x86)']}/Microsoft/Edge/Application/msedge.exe`, '/usr/bin/chromium'].filter(Boolean)) {
  try { await access(candidate); executablePath = candidate; break; } catch {}
}
assert.ok(executablePath, 'Set TEST_BROWSER to an installed Chromium');
const browser = await chromium.launch({ headless: true, executablePath });
try {
  for (const inline of [false, true]) {
    const page = await browser.newPage();
    const errors = [];
    page.on('pageerror', error => errors.push(error.stack));
    await page.setContent('<style>.titlebar-staging{display:none}</style><div id="root"></div>');
    await page.evaluate(inline => { window.inline = inline; }, inline);
    await page.addScriptTag({ type: 'module', content: compiled.outputFiles[0].text });
    const parent = inline ? 'main' : '.titlebar-view';
    await page.locator(`${parent} .page-header span`).filter({ hasText: 'first header' }).waitFor();
    assert.equal(await page.locator('.page-header').count(), 1, 'initial batched refs do not duplicate a page header');
    const input = await page.locator(`${parent} .page-header input`).elementHandle();
    await page.evaluate(() => check.setName('updated header'));
    await page.locator(`${parent} .page-header span`).filter({ hasText: 'updated header' }).waitFor();
    assert.equal(await page.locator(`${parent} .page-header input`).evaluate((element, original) => element === original, input), true);
    await page.evaluate(() => check.setShown(false));
    await page.locator('.page-header').waitFor({ state: 'detached' });
    await page.evaluate(() => check.setShown(true));
    await page.locator(`${parent} .page-header span`).filter({ hasText: 'updated header' }).waitFor();
    assert.deepEqual(errors, []);
    await page.close();
  }
  console.log('Titlebar Portal: initial batched refs, explicit staging, reactive content, cleanup/remount and inline fallback passed.');
} finally {
  await browser.close();
}
