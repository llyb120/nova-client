import assert from 'node:assert/strict';
import { writeFile, rm } from 'node:fs/promises';
import { createServer } from 'vite';
import { chromium } from 'playwright-core';

const name = `sidebar-session-check-${process.pid}`;
const key = 'fd:sidebarLayout';
let server, browser;
try {
  await writeFile(`${name}.html`, `<div id="root"></div><script type="module" src="/${name}.tsx"></script>`);
  await writeFile(`${name}.tsx`, `
    import { render } from '@solidjs/web';
    import { Sidebar } from './src/components/Sidebar';
    import { setState } from './src/store';
    import { moveSidebarSession, setSidebarCollapsed } from './src/sidebarLayout';
    import './src/app.css';
    window.setSidebarState = setState;
    window.setSidebarCollapsed = setSidebarCollapsed;
    window.moveSidebarSession = moveSidebarSession;
    render(() => <div class="app"><Sidebar onOpenSettings={() => {}}
      onOpenAchievements={() => {}} onOpenUpdate={() => {}} onOpenInbox={() => {}} />
      <main style={{ flex: 1 }}>主区域</main></div>, document.getElementById('root'));
  `);
  server = await createServer({ optimizeDeps: { entries: [`${name}.html`] }, server: { host: '127.0.0.1', port: 5199, strictPort: false } });
  await server.listen();
  browser = await chromium.launch({ channel: 'msedge', headless: true });
  const page = await browser.newPage({ viewport: { width: 1300, height: 800 } });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.addInitScript(() => {
    window.__TAURI_INTERNALS__ = { metadata: { currentWindow: { label: 'main' }, currentWebview: { label: 'main' } }, invoke: async command => command === 'plugin:app|version' ? '0.1.704' : null };
  });
  await page.goto(`${server.resolvedUrls.local[0]}${name}.html`, { waitUntil: 'domcontentloaded', timeout: 90000 });
  const shell = page.locator('.sidebar-shell');
  const collapsed = () => shell.evaluate(el => el.classList.contains('collapsed'));
  const stored = () => page.evaluate(key => JSON.parse(localStorage.getItem(key)), key);
  const sessions = async () => (await stored()).collapsedSessions;
  const open = id => page.evaluate(id => window.setSidebarState(s => { s.currentId = id; }), id);
  const collapse = (id, value) => page.evaluate(({ id, value }) => window.setSidebarCollapsed(id, value), { id, value });
  await shell.waitFor({ state: 'attached' });

  assert.equal(await collapsed(), false, 'sessions start expanded');
  await collapse(null, true);
  assert.equal(await collapsed(), true);
  assert.deepEqual(await sessions(), ['']);
  await open('thread-a');
  assert.equal(await collapsed(), false, 'each session keeps its own collapsed state');
  await collapse('thread-a', true);
  assert.equal(await collapsed(), true);
  await open('thread-b');
  assert.equal(await collapsed(), false);
  await open(null);
  assert.equal(await collapsed(), true, 'home keeps its own collapsed state');
  assert.deepEqual(await sessions(), ['', 'thread-a']);

  await page.reload({ waitUntil: 'domcontentloaded' });
  await shell.waitFor({ state: 'attached' });
  await open('thread-a');
  assert.equal(await collapsed(), true, 'collapsed sessions survive a reload');
  await open('thread-b');
  assert.equal(await collapsed(), false);

  await collapse('draft', true);
  await page.evaluate(() => window.moveSidebarSession('draft', 'created'));
  await open('created');
  assert.equal(await collapsed(), true, 'draft handoff keeps the collapsed state');
  assert.deepEqual(await sessions(), ['', 'thread-a', 'created'], 'the draft key is dropped on handoff');

  await page.evaluate(key => localStorage.setItem(key, JSON.stringify({ collapsedSessions: ['a', null, 42, {}] })), key);
  await page.reload({ waitUntil: 'domcontentloaded' });
  await shell.waitFor({ state: 'attached' });
  await open('a');
  assert.equal(await collapsed(), true);
  await open('b');
  assert.equal(await collapsed(), false);
  await page.evaluate(() => window.setSidebarCollapsed('b', false));
  assert.deepEqual(await sessions(), ['a'], 'invalid stored session entries are discarded');
  assert.deepEqual(errors, []);
  console.log('sidebar collapse is stored per session, survives reload, migrates draft ids and validates storage');
} finally {
  await browser?.close();
  await server?.close();
  await Promise.all([`${name}.html`, `${name}.tsx`].map(path => rm(path, { force: true })));
}
