// Real sidebar/titlebar layout and update clicks; native updates are mocked.
import assert from 'node:assert/strict';
import { writeFile, rm } from 'node:fs/promises';
import { createServer } from 'vite';
import { chromium } from 'playwright-core';

const name = `sidebar-version-check-${process.pid}`;
let server, browser;
try {
  await writeFile(`${name}.html`, `<div id="root"></div><script type="module" src="/${name}.tsx"></script>`);
  await writeFile(`${name}.tsx`, `
    import { createSignal, Show } from 'solid-js';
    import { render } from '@solidjs/web';
    import { TitleBar } from './src/components/TitleBar';
    import { Sidebar } from './src/components/Sidebar';
    import { setState } from './src/store';
    import '@fontsource-variable/inter';
    import './src/app.css';
    const [shown, setShown] = createSignal(true);
    window.setSidebarState = setState;
    window.setShown = setShown;
    render(() => <><TitleBar /><Show when={shown()}><Sidebar onOpenSettings={() => {}}
      onOpenAchievements={() => {}} onOpenUpdate={() => {}} onOpenInbox={() => {}} /></Show></>,
      document.getElementById('root'));
  `);
  server = await createServer({ optimizeDeps: { entries: [`${name}.html`] }, server: { host: '127.0.0.1', port: 0 } });
  await server.listen();
  browser = await chromium.launch({ channel: 'msedge', headless: true });
  const page = await browser.newPage({ viewport: { width: 1100, height: 700 } });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.addInitScript(() => {
    window.updateCalls = [];
    window.__TAURI_INTERNALS__ = {
      metadata: { currentWindow: { label: 'main' }, currentWebview: { label: 'main' } },
      transformCallback: () => 0,
      unregisterCallback: () => {},
      invoke: async command => {
        if (command === 'plugin:app|version') return '0.1.684';
        if (command === 'check_update' || command === 'download_staged_update') window.updateCalls.push(command);
        if (command === 'check_update') return { hasUpdate: true, staged: false, version: '0.1.685' };
        if (command === 'download_staged_update') return { ready: true };
        return null;
      },
    };
  });
  await page.goto(`${server.resolvedUrls.local[0]}${name}.html`, { waitUntil: 'domcontentloaded', timeout: 90000 });
  await page.evaluate(() => {
    window.setSidebarState(s => {
      Object.assign(s.relay, { enabled: true, connected: true });
      s.peers = Array.from({ length: 3 }, (_, i) => ({ token: String(i), name: String(i), online: true }));
    });
  });
  const version = page.locator('.brand-version');
  await version.waitFor();
  await page.evaluate(() => document.fonts.ready);
  const inline = expected => page.waitForFunction(expected =>
    document.querySelector('.brand-version')?.classList.contains('is-inline') === expected, expected);
  await inline(true);
  assert.equal(await version.textContent(), 'v0.1.684');
  assert.ok(await version.evaluate(button => button.getBoundingClientRect().right <=
    button.closest('.brand').querySelector('.relay-badge-wrap').getBoundingClientRect().left - 8),
    'the pictured 248px header fits the version without overlapping online members');
  await version.click();
  await page.waitForFunction(() => window.updateCalls.length === 2);
  assert.deepEqual(await page.evaluate(() => window.updateCalls), ['check_update', 'download_staged_update']);

  await page.evaluate(() => window.setSidebarState(s => { s.inbox = [{ id: 'inbox' }]; }));
  await inline(false);
  await page.mouse.move(700, 500);
  await page.waitForFunction(() => getComputedStyle(document.querySelector('.brand-version')).opacity === '0');
  await page.locator('.brand-name').hover();
  await page.waitForFunction(() => Number(getComputedStyle(document.querySelector('.brand-version')).opacity) > 0);
  await version.click();
  await page.waitForFunction(() => window.updateCalls.length === 4);
  assert.deepEqual(await page.evaluate(() => window.updateCalls.slice(2)), ['check_update', 'download_staged_update'],
    'hover-only versions retain automatic check and download');

  await page.mouse.move(700, 500);
  await page.waitForFunction(() => getComputedStyle(document.querySelector('.brand-version')).opacity === '0');
  await page.keyboard.press('Tab');
  await version.focus();
  assert.equal(await version.evaluate(button => button.matches(':focus-visible')), true);
  await page.waitForFunction(() => Number(getComputedStyle(document.querySelector('.brand-version')).opacity) > 0);
  await page.locator('.sidebar-head-search').focus();

  await page.locator('.titlebar-side').evaluate(element => { element.style.width = '340px'; });
  await inline(true);
  await page.locator('.titlebar-side').evaluate(element => { element.style.removeProperty('width'); });
  await inline(false);
  await page.evaluate(() => window.setSidebarState(s => { s.inbox = []; s.update = null; }));
  await inline(true);
  await version.evaluate(element => { element.style.fontSize = '24px'; });
  await inline(false);
  await version.evaluate(element => { element.style.removeProperty('font-size'); });
  await inline(true);

  await page.locator('.sidebar-head-toggle[aria-controls="main-sidebar"]').click();
  await page.locator('.titlebar-side.collapsed').waitFor();
  assert.equal(await version.isVisible(), false);
  await page.locator('.sidebar-rail-pin').click();
  await inline(true);
  await version.waitFor();
  await page.evaluate(() => window.setShown(false));
  await version.waitFor({ state: 'detached' });
  await page.evaluate(() => window.setShown(true));
  await version.waitFor();
  await inline(true);
  assert.deepEqual(errors, []);
  console.log('Sidebar version: intrinsic fit, dynamic badges/width/font, hover, keyboard, update clicks and remount passed.');
} finally {
  await browser?.close();
  await server?.close();
  await Promise.all([`${name}.html`, `${name}.tsx`].map(path => rm(path, { force: true })));
}
