import assert from 'node:assert/strict';
import { writeFile, rm } from 'node:fs/promises';
import { createServer } from 'vite';
import { chromium } from 'playwright-core';

const name = `sidebar-check-${process.pid}`;
let server, browser;
try {
  await writeFile(`${name}.html`, `<div id="root"></div><script type="module" src="/${name}.tsx"></script>`);
  await writeFile(`${name}.tsx`, `
    import { render } from 'solid-js/web';
    import { Sidebar } from './src/components/Sidebar';
    import { setState } from './src/store';
    window.setSidebarState = setState;
    import './src/app.css';
    window.settingsOpened = 0;
    render(() => <div class="app"><Sidebar onOpenSettings={() => { window.settingsOpened++; }}
      onOpenAchievements={() => {}} onOpenUpdate={() => {}} onOpenInbox={() => {}} />
      <main style={{ flex: 1 }}><button id="content">主区域</button></main></div>, document.getElementById('root'));
  `);
  server = await createServer({ server: { host: '127.0.0.1', port: 5199, strictPort: false } });
  await server.listen();
  browser = await chromium.launch({ channel: 'msedge', headless: true });
  const page = await browser.newPage({ viewport: { width: 1300, height: 800 } });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.addInitScript(() => {
    window.__TAURI_INTERNALS__ = { invoke: async command => command === 'plugin:app|version' ? 'test' : null };
  });
  await page.goto(`${server.resolvedUrls.local[0]}${name}.html`, { waitUntil: 'domcontentloaded', timeout: 90000 });
  const sidebar = page.locator('#main-sidebar');
  const headToggle = page.locator('.sidebar-head-toggle');
  const railPin = page.locator('.sidebar-rail-pin');
  await sidebar.waitFor({ state: 'visible' });
  const contentLeft = () => page.locator('main').evaluate(el => el.getBoundingClientRect().left);
  const pinnedLeft = await contentLeft();
  assert.ok(pinnedLeft >= 248);
  await headToggle.click();
  await sidebar.waitFor({ state: 'hidden' });
  assert.equal(await contentLeft(), 52, 'collapsed sidebar keeps only the compact icon rail');
  assert.equal(await sidebar.evaluate(el => el.inert), true);
  const rail = page.getByRole('navigation', { name: '快捷导航' });
  const runningBadge = rail.locator('.sidebar-rail-count.running');
  const unreadBadge = rail.locator('.sidebar-rail-count.unread');
  assert.equal(await rail.locator('.sidebar-rail-count').count(), 0);
  await page.evaluate(() => {
    const thread = (id, parentThreadId) => ({ id, parentThreadId, title: id, cwd: 'D:/project',
      agentKind: 'codex', createdAt: 1, updatedAt: 1, running: false, starred: false, unreadTurns: 0 });
    window.setSidebarState('threads', [thread('root'), thread('child', 'root'), thread('done'),
      { ...thread('legacy'), experienceThread: true }]);
    window.setSidebarState('running', { root: true, child: true });
    window.setSidebarState('unreadTurns', { root: 4, child: 2, done: 3, legacy: 5, deleted: 8 });
  });
  assert.equal(await runningBadge.textContent(), '1', 'parent and child count as one running task');
  assert.equal(await unreadBadge.textContent(), '3', 'only completed visible threads contribute unread turns');
  await page.evaluate(() => window.setSidebarState('running', { root: false, child: false }));
  assert.equal(await runningBadge.count(), 0);
  assert.equal(await unreadBadge.textContent(), '9', 'completion moves unread turns into the badge');
  await page.evaluate(() => window.setSidebarState('unreadTurns', { root: 0, child: 0, done: 120 }));
  assert.equal(await unreadBadge.textContent(), '99+');
  assert.equal(await unreadBadge.getAttribute('aria-label'), '已完成未读：120');
  await page.evaluate(() => window.setSidebarState('currentId', 'done'));
  assert.equal(await unreadBadge.count(), 0, 'currently read conversation is excluded');
  await page.evaluate(() => {
    window.setSidebarState('currentId', null);
    window.setSidebarState('threads', []);
  });
  await rail.getByRole('button', { name: '工作流', exact: true }).click();
  assert.equal(await rail.getByRole('button', { name: '工作流', exact: true }).evaluate(el => el.classList.contains('active')), true);
  assert.equal(await sidebar.isVisible(), false, 'routine icon actions do not force the full sidebar open');
  await rail.getByRole('button', { name: '证据链', exact: true }).click();
  assert.equal(await rail.getByRole('button', { name: '证据链', exact: true }).evaluate(el => el.classList.contains('active')), true);
  await rail.getByRole('button', { name: '设置', exact: true }).click();
  assert.equal(await page.evaluate(() => window.settingsOpened), 1);
  await rail.getByRole('button', { name: '新对话', exact: true }).click();
  assert.equal(await rail.locator('button.active').count(), 0);
  await page.mouse.move(51, 300);
  await sidebar.waitFor({ state: 'visible' });
  assert.equal(await contentLeft(), 52, 'hover expansion overlays without shifting content');
  await page.mouse.move(120, 300);
  assert.equal(await sidebar.isVisible(), true, 'moving into the sidebar keeps it open');
  await page.mouse.move(600, 300);
  await sidebar.waitFor({ state: 'hidden' });
  const listBox = await rail.getByRole('button', { name: '会话列表', exact: true }).boundingBox();
  for (const [x, y] of [[26, 300], [listBox.x + listBox.width / 2, listBox.y + listBox.height / 2]]) {
    await page.mouse.move(x, y);
    assert.equal(await sidebar.isVisible(), false, 'passing over the rail must not open immediately');
    await page.mouse.move(-10, 300);
    await page.waitForTimeout(250);
    assert.equal(await sidebar.isVisible(), false, 'leaving the window cancels pending hover expansion');
    await page.mouse.move(600, 300);
  }
  for (const [x, y] of [[26, 300], [1, 400], [43, 500], [51, 600]]) {
    // 单步移入，确保不依赖途中恰好命中原来的 8px 窄边。
    await page.mouse.move(x, y);
    await sidebar.waitFor({ state: 'visible' });
    assert.equal(await contentLeft(), 52, 'blank rail hover overlays without shifting content');
    await page.mouse.move(120, y);
    assert.equal(await sidebar.isVisible(), true, 'rail-to-panel movement keeps the sidebar open');
    await page.mouse.move(600, y);
    await sidebar.waitFor({ state: 'hidden' });
  }
  await railPin.focus();
  await page.keyboard.press('Enter');
  await sidebar.waitFor({ state: 'visible' });
  assert.equal(await contentLeft(), pinnedLeft, 'keyboard activation restores pinned layout');
  await headToggle.focus();
  await page.keyboard.press('Enter');
  await sidebar.waitFor({ state: 'hidden' });
  await railPin.focus();
  assert.equal(await railPin.evaluate(el => el === document.activeElement), true,
    'compact navigation stays keyboard accessible');
  await rail.getByRole('button', { name: '会话列表', exact: true }).hover();
  await sidebar.waitFor({ state: 'visible' });
  await headToggle.click();
  await page.mouse.move(600, 300);
  assert.equal(await contentLeft(), pinnedLeft, 'clicking the hover toggle pins the sidebar');
  assert.equal(await sidebar.isVisible(), true);
  assert.deepEqual(errors, []);
  console.log('sidebar icon actions / hover overlay / pin / keyboard checks passed');
} finally {
  await browser?.close();
  await server?.close();
  await Promise.all([`${name}.html`, `${name}.tsx`].map(path => rm(path, { force: true })));
}
