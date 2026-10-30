import assert from 'node:assert/strict';
import { writeFile, rm } from 'node:fs/promises';
import { createServer } from 'vite';
import { chromium } from 'playwright-core';

const name = `sidebar-project-check-${process.pid}`;
const key = 'fd:sidebarLayout';
const cwd = 'D:/alpha/project';
const otherCwd = 'D:/beta/project';
let server, browser;
try {
  await writeFile(`${name}.html`, `<div id="root"></div><script type="module" src="/${name}.tsx"></script>`);
  await writeFile(`${name}.tsx`, `
    import { render } from 'solid-js/web';
    import { Sidebar } from './src/components/Sidebar';
    import { setState } from './src/store';
    import { setSidebarLayout } from './src/sidebarLayout';
    import './src/app.css';
    window.setSidebarState = setState;
    window.setSidebarLayout = setSidebarLayout;
    setState('threads', Array.from({ length: 8 }, (_, index) => ({
      id: 'thread-' + index, title: 'thread-' + index,
      cwd: index < 7 ? '${cwd}' : '${otherCwd}', agentKind: 'codex',
      createdAt: 1, updatedAt: 1, running: false, starred: false, unreadTurns: 0,
    })));
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
    window.__TAURI_INTERNALS__ = {
      metadata: { currentWindow: { label: 'main' }, currentWebview: { label: 'main' } },
      invoke: async command => command === 'plugin:app|version' ? '0.1.667' : false,
    };
  });
  await page.goto(`${server.resolvedUrls.local[0]}${name}.html`, { waitUntil: 'domcontentloaded', timeout: 90000 });
  const groups = page.locator('.thread-group');
  const first = groups.nth(0), second = groups.nth(1);
  const firstToggle = first.locator('.group-project-toggle');
  const secondToggle = second.locator('.group-project-toggle');
  const stored = () => page.evaluate(key => JSON.parse(localStorage.getItem(key)), key);
  await firstToggle.waitFor({ state: 'visible' });
  assert.deepEqual(await groups.locator('.group-name').allTextContents(), ['project', 'project']);
  assert.equal(await firstToggle.getAttribute('aria-expanded'), 'true');
  assert.equal(await first.locator('.thread-item').count(), 5);
  assert.equal(await second.locator('.thread-item').count(), 1);
  assert.equal(await firstToggle.locator('button').count(), 0, 'project and delete actions are separate buttons');
  await first.locator('.thread-group-toggle').click();
  assert.equal(await first.locator('.thread-item').count(), 7, 'show more retains its existing behavior');

  await firstToggle.click();
  assert.equal(await firstToggle.getAttribute('aria-expanded'), 'false');
  assert.equal(await first.locator('.thread-item').count(), 0);
  assert.equal(await first.locator('.thread-group-toggle').count(), 0);
  assert.equal(await second.locator('.thread-item').count(), 1, 'same-named projects are independent');
  assert.deepEqual(await stored(), { collapsed: false, collapsedProjects: [cwd] });
  await firstToggle.focus();
  await page.keyboard.press('Enter');
  assert.equal(await first.locator('.thread-item').count(), 7, 'reopening keeps the current show-more state');
  await page.keyboard.press('Space');
  assert.equal(await first.locator('.thread-item').count(), 0, 'Space can collapse the project');
  await secondToggle.click();
  assert.deepEqual((await stored()).collapsedProjects, [cwd, otherCwd]);

  await page.reload({ waitUntil: 'domcontentloaded' });
  await firstToggle.waitFor({ state: 'visible' });
  assert.equal(await firstToggle.getAttribute('aria-expanded'), 'false');
  assert.equal(await secondToggle.getAttribute('aria-expanded'), 'false');
  assert.equal(await groups.locator('.thread-item').count(), 0, 'both project states survive a fresh module load');
  await firstToggle.click();
  assert.equal(await first.locator('.thread-item').count(), 5);
  assert.equal(await second.locator('.thread-item').count(), 0);
  await page.evaluate(() => window.setSidebarLayout({ collapsed: true }));
  assert.deepEqual(await stored(), { collapsed: true, collapsedProjects: [otherCwd] });
  await page.evaluate(() => window.setSidebarLayout({ collapsed: false }));
  await page.evaluate(() => window.setSidebarState('settings', { historyDisplayMode: 'time' }));
  assert.equal(await groups.count(), 0);
  assert.equal(await page.locator('.thread-list .thread-item').count(), 8, 'time view ignores project folds');
  await page.evaluate(() => window.setSidebarState('settings', { historyDisplayMode: 'project' }));
  assert.equal(await second.locator('.thread-item').count(), 0, 'returning to projects preserves their state');

  await page.evaluate(key => localStorage.setItem(key, JSON.stringify({ collapsed: false })), key);
  await page.reload({ waitUntil: 'domcontentloaded' });
  await firstToggle.waitFor({ state: 'visible' });
  assert.equal(await first.locator('.thread-item').count(), 5, 'old layouts default to expanded projects');
  assert.equal(await second.locator('.thread-item').count(), 1);
  await page.evaluate(({ key, cwd }) => localStorage.setItem(key, JSON.stringify({
    collapsed: false, collapsedProjects: [cwd, null, false, 42, {}],
  })), { key, cwd });
  await page.reload({ waitUntil: 'domcontentloaded' });
  await firstToggle.waitFor({ state: 'visible' });
  assert.equal(await first.locator('.thread-item').count(), 0);
  assert.equal(await second.locator('.thread-item').count(), 1);
  await page.evaluate(() => window.setSidebarLayout({ collapsed: false }));
  assert.deepEqual((await stored()).collapsedProjects, [cwd], 'invalid stored project entries are discarded');
  await page.evaluate(key => localStorage.setItem(key, '{'), key);
  await page.reload({ waitUntil: 'domcontentloaded' });
  await firstToggle.waitFor({ state: 'visible' });
  assert.equal(await first.locator('.thread-item').count(), 5, 'corrupt storage falls back to the default layout');
  assert.deepEqual(errors, []);
  console.log('sidebar project collapse / keyboard / persistence / layout validation checks passed');
} finally {
  await browser?.close();
  await server?.close();
  await Promise.all([`${name}.html`, `${name}.tsx`].map(path => rm(path, { force: true })));
}
