// Real Solid 2 DOM/lifecycle, isolated browser; no native window operations.
import assert from 'node:assert/strict';
import { access, readFile } from 'node:fs/promises';
import { build } from 'esbuild';
import { transform } from '@solidjs/compiler';
import { chromium } from 'playwright-core';

const chat = (await readFile(new URL('../src/components/ChatView.tsx', import.meta.url), 'utf8')).replace(/\r\n/g, '\n');
const css = await readFile(new URL('../src/app.css', import.meta.url), 'utf8');
const setupStart = chat.indexOf('  const worldlineOpen =');
const setupEnd = chat.indexOf('\n  return (\n    <main', setupStart);
const actionHeader = '        <div class="chat-actions">\n';
const actionsStart = chat.indexOf(actionHeader);
const actionsEnd = chat.indexOf('        </div>\n      </header>\n      </TitleBarPortal>', actionsStart);
const formatStart = chat.indexOf('function fmtSessionCost(');
const usageStart = chat.indexOf('  const tokenStats =');
const usageEnd = chat.indexOf('  onCleanup(stopUsageRoll);', usageStart);
assert.ok(setupStart >= 0 && setupEnd > setupStart && actionsStart >= 0 && actionsEnd > actionsStart);
assert.ok(formatStart >= 0 && usageStart >= 0 && usageEnd > usageStart);
const setup = chat.slice(setupStart, setupEnd);
const actions = chat.slice(actionsStart + actionHeader.length, actionsEnd);
const format = chat.slice(formatStart, chat.indexOf('\n}', formatStart) + 2);
const usage = chat.slice(usageStart, chat.indexOf('\n', usageEnd));

const fixture = `
  import { createEffect, createMemo, createSignal, createStore, flush, onCleanup, onSettled, Show, untrack } from 'solid-js';
  import { render } from '@solidjs/web';
  import { TitleBar, TitleBarPortal } from './src/components/TitleBar';
  import { fmtTokens } from './src/components/TurnGroup';
  import { IconChevron, IconCopy, IconDownload, IconPanelRight, IconShare, IconStopwatch } from './src/components/icons';
  const [shown, setShown] = createSignal(true);
  const [name, setName] = createSignal('first header');
  const [workspaceOpen, setWorkspaceOpen] = createSignal(false);
  const [timeMachineExpanded, setTimeMachineExpanded] = createSignal(false);
  const [previewItems, setPreviewItems] = createSignal(null);
  const events = [];
  const [state, setState] = createStore({ currentId: 'first', relay: { connected: true }, loadingThread: false,
    items: [{ type: 'turn', totalTokens: 1600000, costUsd: 1.51 }], liveUsage: null });
  const displayedItems = () => previewItems() ?? state.items;
  const timeMachineChangedSignal = () => 0;
  const api = { priceSession: async () => null };
  ${format}
  const currentMeta = () => ({}), canShare = () => true, showTimeMachine = () => true;
  const isSubagent = () => false, roamingRole = () => null, timeMachineHintTurn = () => 0;
  const forkUnavailable = () => false, exportBusy = () => false, recalling = () => false;
  const setShowShare = () => events.push('Flow'), startFork = () => events.push('Fork');
  const exportSession = () => events.push('导出'), recall = () => events.push('召回');
  function Actions() {
    ${usage}
    ${setup}
    return <div class="chat-actions">${actions}</div>;
  }
  const update = patch => flush(() => setState(s => { Object.assign(s, patch); }));
  const settle = () => flush(() => setState(s => { s.items.push({ type: 'turn', ...s.liveUsage }); s.liveUsage = null; }));
  window.check = { setShown, setName, setId: currentId => update({ currentId }),
    workspaceOpen, timeMachineExpanded, events, update, settle, preview: items => flush(() => setPreviewItems(items)) };
  render(() => <>
    {!window.inline && <TitleBar />}
    <main><Show when={shown()}><TitleBarPortal>
      <header class="page-header chat-head"><input value={name()} /><span>{name()}</span><Actions /></header>
    </TitleBarPortal></Show><button type="button" id="outside">outside</button></main>
  </>, document.getElementById('root'));
`;
const compiled = await build({
  stdin: { contents: transform(fixture, { filename: 'fixture.tsx', dev: true }).code, resolveDir: process.cwd(), loader: 'ts' },
  bundle: true, write: false, platform: 'browser', format: 'esm', conditions: ['browser', 'development'],
  plugins: [{ name: 'native-window-fixture', setup(builder) {
    builder.onResolve({ filter: /^@tauri-apps\/api\/window$/ }, () => ({ path: 'window', namespace: 'fixture' }));
    builder.onResolve({ filter: /^\.\.\/sidebarLayout$/ }, () => ({ path: 'sidebar', namespace: 'fixture' }));
    builder.onResolve({ filter: /^\.\.\/store$/ }, () => ({ path: 'store', namespace: 'fixture' }));
    builder.onLoad({ filter: /.*/, namespace: 'fixture' }, ({ path }) => ({ loader: 'js', contents: {
      sidebar: 'export const isSidebarCollapsed = () => false;',
      store: 'export const state = { currentId: null };',
      window: 'export const getCurrentWindow = () => ({ isMaximized: async () => false, onResized: async () => () => {} });',
    }[path] }));
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
    await page.setContent(`<style>${css}.titlebar-staging{display:none}</style><div id="root"></div>`);
    await page.evaluate(inline => { window.inline = inline; }, inline);
    await page.addScriptTag({ type: 'module', content: compiled.outputFiles[0].text });
    const parent = inline ? 'main' : '.titlebar-view';
    await page.locator(`${parent} .page-header span`).filter({ hasText: 'first header' }).waitFor();
    assert.equal(await page.locator('.page-header').count(), 1, 'initial batched refs do not duplicate a page header');
    const input = await page.locator(`${parent} .page-header input`).elementHandle();
    await page.evaluate(() => check.setName('updated header'));
    await page.locator(`${parent} .page-header span`).filter({ hasText: 'updated header' }).waitFor();
    assert.equal(await page.locator(`${parent} .page-header input`).evaluate((element, original) => element === original, input), true);

    const trigger = page.locator(`${parent} .chat-action-menu > summary`);
    const list = page.locator('.chat-action-list');
    const sidebar = page.getByRole('button', { name: '打开右侧栏', exact: true });
    assert.equal(await list.isVisible(), false);
    assert.equal(await sidebar.isVisible(), true, 'sidebar toggle is outside the dropdown');
    assert.equal((await trigger.boundingBox()).height, (await sidebar.boundingBox()).height);
    await trigger.press('Space');
    await list.waitFor();
    assert.ok((await list.boundingBox()).y >= (await trigger.boundingBox()).y + 28, 'panel opens below the titlebar button');
    await page.keyboard.press('Tab');
    assert.equal(await page.evaluate(() => document.activeElement?.getAttribute('aria-label')), '线索与 Flow 分享');
    await page.keyboard.press('Escape');
    assert.equal(await list.isVisible(), false);
    assert.equal(await trigger.evaluate(element => document.activeElement === element), true);
    await trigger.click();
    await page.locator('#outside').click();
    assert.equal(await list.isVisible(), false);

    for (const label of ['线索与 Flow 分享', 'Fork 整条会话', '导出会话']) {
      await trigger.click();
      await list.getByRole('button', { name: label, exact: true }).click();
      assert.equal(await list.isVisible(), false);
    }
    assert.deepEqual(await page.evaluate(() => check.events), ['Flow', 'Fork', '导出']);
    await sidebar.click();
    assert.equal(await list.isVisible(), false);
    await trigger.click();
    await list.getByRole('button', { name: '展开世界线', exact: true }).click();
    assert.deepEqual(await page.evaluate(() => [check.workspaceOpen(), check.timeMachineExpanded()]), [false, true]);
    await trigger.click();
    await list.getByRole('button', { name: '收起世界线', exact: true }).click();
    assert.equal(await page.evaluate(() => check.timeMachineExpanded()), false);
    await trigger.click();
    await page.evaluate(() => check.setId('other'));
    await list.waitFor({ state: 'hidden' });
    await trigger.click();
    await page.evaluate(() => check.setShown(false));
    await page.locator('.page-header').waitFor({ state: 'detached' });
    await page.evaluate(() => check.setShown(true));
    await page.locator(`${parent} .page-header span`).filter({ hasText: 'updated header' }).waitFor();
    if (!inline) {
      // Keep the roll deterministic, but verify the hop with Chromium's real animation engine.
      await page.clock.install({ time: new Date(0) });
      await page.clock.pauseAt(new Date(60_000));
      const badge = page.locator(`${parent} .chat-tokens`);
      assert.equal(await badge.textContent(), '1.6M tokens · $1.51');
      assert.deepEqual(await badge.evaluate(el => {
        const color = getComputedStyle(el).color;
        check.update({ liveUsage: { totalTokens: 240000, costUsd: 0.36 } });
        const hop = el.getAnimations()[0];
        if (!hop) return null;
        hop.pause(); hop.currentTime = 115;
        const style = getComputedStyle(el);
        const result = [style.transform !== 'none', style.color !== color, el.title.endsWith('模型估价 $1.87')];
        hop.play();
        return result;
      }), [true, true, true], 'request settlement hops and highlights; the tooltip already shows the real estimate');
      await page.clock.runFor(96);
      const mid = await badge.textContent();
      assert.match(mid, /^1\.7M tokens · \$1\.[67]\d$/, 'both real values interpolate');
      assert.deepEqual(await badge.evaluate(el => {
        check.update({ liveUsage: { totalTokens: 600000, costUsd: 0.72, unknownRequests: 1 } });
        const hop = el.getAnimations()[0];
        check.settle();
        return [el.textContent, !!hop && el.getAnimations()[0] === hop];
      }), [mid.replace('$', '≥$'), true], 'a second settlement continues from the visible number and persistence does not replay it');
      await page.clock.runFor(500);
      assert.equal(await badge.textContent(), '2.2M tokens · ≥$2.23');
      assert.deepEqual(await badge.evaluate(el => {
        check.update({ liveUsage: { totalTokens: 1, costUsd: 0.00001 } });
        const hopped = el.getAnimations().length === 1;
        check.preview([{ type: 'turn', totalTokens: 1234 }]);
        const preview = [el.textContent, el.getAnimations().length];
        check.preview(null);
        return [hopped, preview, el.textContent, el.getAnimations().length];
      }), [true, ['1.2k tokens', 0], '2.2M tokens · ≥$2.23', 0], 'sub-display increments still hop, but a timeline preview snaps and hides unavailable cost');
      assert.deepEqual(await badge.evaluate(el => {
        check.update({ liveUsage: { totalTokens: 200000, costUsd: 0.1 } });
        check.update({ currentId: 'next', loadingThread: true, items: [], liveUsage: null });
        check.update({ loadingThread: false, items: [{ type: 'turn', totalTokens: 500, costUsd: 0.2 }] });
        return [el.textContent, el.getAnimations().length];
      }), ['500 tokens · $0.2000', 0], 'loading another session does not animate someone else’s totals');
      await page.emulateMedia({ reducedMotion: 'reduce' });
      assert.deepEqual(await badge.evaluate(el => {
        check.update({ liveUsage: { totalTokens: 20, costUsd: 0.05 } });
        return [el.textContent, el.getAnimations().length];
      }), ['520 tokens · $0.2500', 0], 'reduced motion shows the updated amounts directly');
    }
    assert.deepEqual(errors, []);
    await page.close();
  }
  console.log('Titlebar Portal, chat actions and request-settlement animation passed.');
} finally {
  await browser.close();
}
