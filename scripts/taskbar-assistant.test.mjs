import assert from 'node:assert/strict';
import { access, readdir } from 'node:fs/promises';
import { createServer } from 'vite';
import solid from 'vite-plugin-solid';
import { chromium } from 'playwright-core';

// Real Solid lifecycle/scroll/IME checks, with only the IPC boundary mocked. No test framework.
const entry = `
import { render } from 'solid-js/web';
import TaskbarBubble from '/src/components/TaskbarBubble.tsx';
document.documentElement.classList.add('bubble-mode');
window.calls = []; window.events = {}; window.hover = true; window.card = 'first'; window.failSend = false;
window.historyMessages = Array.from({length:75}, (_,i) => ({id:'m'+i,role:i%2?'assistant':'user',text:'消息 '+i+'\\n第二行',createdAt:i}));
window.emit = name => (window.events[name] || []).forEach(fn => fn({payload:null}));
window.ipc = async (command, args = {}) => {
  window.calls.push([command, args]);
  if (command === 'taskbar_bubble') return {cards:[{key:window.card,live:true,kind:'running',threadId:window.card,title:window.card,text:'最新步骤',body:''}],permissions:[],light:true,panel:window.hover};
  if (command === 'taskbar_bubble_place') return true;
  if (command === 'taskbar_assistant_history') {
    if (window.delayHistory) await new Promise(resolve => { window.resolveHistory = resolve; });
    const end = args.before ? window.historyMessages.findIndex(m => m.id === args.before) : window.historyMessages.length;
    const start = Math.max(0, end - args.limit);
    return { messages: window.historyMessages.slice(start,end), hasMore:start>0, busy:false };
  }
  if (command === 'taskbar_assistant_sessions') return [{threadId:'done',title:'已完成任务',kind:'done',text:'结论',updatedAt:2},{threadId:'running',title:'运行任务',kind:'running',text:'最新一步',updatedAt:1}];
  if (command === 'taskbar_assistant_send') {
    if (window.failSend) throw Error('offline');
    const n = window.historyMessages.length;
    window.historyMessages.push({id:'u'+n,role:'user',text:args.text,createdAt:n});
    window.emit('taskbar-assistant:changed');
    if (window.delaySend) await new Promise(resolve => { window.resolveSend = resolve; });
    const result = {id:'a'+n,role:'assistant',text:'已安排',createdAt:n+1,threadId:'running'};
    window.historyMessages.push(result); window.emit('taskbar-assistant:changed'); return result;
  }
};
render(() => <TaskbarBubble />, document.getElementById('root'));
`;
let server, browser;
try {
  server = await createServer({
    configFile: false,
    logLevel: 'error',
    optimizeDeps: { noDiscovery: true, include: ['solid-js', 'solid-js/web', 'solid-js/store', 'dompurify', 'marked'] },
    plugins: [solid(), {
      name: 'taskbar-check',
      enforce: 'pre',
      resolveId(id) {
        if (id === '@tauri-apps/api/core') return '\0check-core';
        // Parallel capsule work is outside this check; keep it mounted but do not invoke its APIs.
        if (id === './ThoughtCapsules') return '\0check-capsules';
        if (id === '@tauri-apps/api/event') return '\0check-event';
        if (id === '/__taskbar_check.tsx') return id;
      },
      load(id) {
        if (id === '\0check-core') return 'export const invoke = (...args) => window.ipc(...args); export const convertFileSrc = path => path;';
        if (id === '\0check-capsules') return 'export const captureCapsule = async () => {}; export default function ThoughtCapsules() { return null; }';
        if (id === '\0check-event') return 'export const listen = async (name, fn) => { (window.events[name] ||= []).push(fn); return () => window.events[name].splice(window.events[name].indexOf(fn),1); };';
        if (id === '/__taskbar_check.tsx') return entry;
      },
      configureServer(server) {
        server.middlewares.use((req, res, next) => {
          if (req.url !== '/__taskbar_check') return next();
          res.setHeader('Content-Type', 'text/html');
          res.end('<div id="root"></div><script type="module" src="/__taskbar_check.tsx"></script>');
        });
      },
    }],
    server: { host: '127.0.0.1', port: 0, watch: null },
  });
  await server.listen();
  const cache = `${process.env.HOME}/.cache/ms-playwright`;
  const shells = (await readdir(cache).catch(() => [])).filter(name => name.startsWith('chromium_headless_shell-')).map(name => `${cache}/${name}/chrome-headless-shell-linux64/chrome-headless-shell`);
  const candidates = [process.env.TEST_BROWSER, chromium.executablePath(), ...shells, '/usr/bin/chromium', 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe'].filter(Boolean);
  let executablePath;
  for (const path of candidates) { try { await access(path); executablePath = path; break; } catch {} }
  assert.ok(executablePath, 'Set TEST_BROWSER to a Chromium executable');
  browser = await chromium.launch({ executablePath, headless: true });
  const page = await browser.newPage({ viewport: { width: 560, height: 640 } });
  page.setDefaultTimeout(5000);
  const errors = [];
  page.on('pageerror', error => { errors.push(error.message); console.error(error.message); });
  page.on('console', message => { if (message.type() === 'error') console.error(message.text()); });
  await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/__taskbar_check`);
  await page.waitForSelector('[data-message-id="m74"]');
  const input = page.getByRole('textbox', { name: '给助理的消息' });
  assert.equal(await input.evaluate(el => el === document.activeElement), false, 'hover cannot focus');
  assert.equal(await page.getByRole('tab', { name: '助理', exact: true }).getAttribute('aria-selected'), 'true');
  assert.equal(await page.locator('.ta-message').count(), 30, 'initially only newest page');
  assert.equal(await page.locator('.ta-session').count(), 0, 'session activity lives in the 会话 tab');
  assert.equal(await page.locator('.tb-card').evaluate(el => getComputedStyle(el).animationName), 'none');
  await page.waitForFunction(() => { const el = document.querySelector('.ta-history'); return el.scrollHeight - el.clientHeight - el.scrollTop < 2; });
  const node = await input.elementHandle();
  await input.fill('保留草稿');
  assert.equal(await page.evaluate(() => window.calls.some(([cmd]) => cmd === 'taskbar_bubble_activate')), true);
  await page.getByRole('tab', { name: '快捷', exact: true }).click();
  await page.getByRole('button', { name: '知识图谱' }).waitFor();
  await page.getByRole('tab', { name: '会话', exact: true }).click();
  await page.getByRole('button', { name: '打开会话', exact: true }).waitFor();
  await page.evaluate(() => { window.card = 'replacement'; });
  await page.waitForFunction(() => document.querySelector('.tb-title')?.textContent === 'replacement');
  await page.getByRole('tab', { name: '助理', exact: true }).click();
  assert.equal(await input.inputValue(), '保留草稿');
  assert.equal(await input.evaluate((el, old) => el === old, node), true, 'tab/card updates cannot remount assistant');
  await page.evaluate(() => { window.emit('taskbar-bubble:hidden'); window.hover = false; window.emit('taskbar-bubble:open'); });
  await page.waitForFunction(() => document.activeElement?.classList.contains('ta-input'));
  assert.equal(await input.evaluate((el, old) => el === old, node), true, 'native hide/open cannot remount assistant');
  assert.equal(await input.inputValue(), '保留草稿');

  // Resolve an upward page while the reader moves: the visible row, not a stale scrollTop, anchors it.
  await page.evaluate(() => { window.delayHistory = true; const el = document.querySelector('.ta-history'); el.scrollTop = 20; el.dispatchEvent(new Event('scroll')); });
  await page.waitForFunction(() => typeof window.resolveHistory === 'function');
  const anchor = await page.evaluate(() => {
    const el = document.querySelector('.ta-history'); el.scrollTop = 80; el.dispatchEvent(new Event('scroll'));
    const top = el.getBoundingClientRect().top;
    const row = [...document.querySelectorAll('.ta-message')].find(row => row.getBoundingClientRect().bottom > top);
    return { id:row.dataset.messageId, offset:row.getBoundingClientRect().top - top };
  });
  await page.evaluate(() => { window.delayHistory = false; window.resolveHistory(); });
  await page.waitForFunction(() => document.querySelectorAll('.ta-message').length === 60);
  const offset = await page.locator(`[data-message-id="${anchor.id}"]`).evaluate(el => el.getBoundingClientRect().top - document.querySelector('.ta-history').getBoundingClientRect().top);
  assert.ok(Math.abs(offset - anchor.offset) < 2, 'prepending keeps the current visual anchor');
  await page.evaluate(() => {
    window.historyMessages.push({id:'new',role:'assistant',text:'新消息',createdAt:100}); window.emit('taskbar-assistant:changed');
  });
  await page.waitForSelector('[data-message-id="new"]', { state: 'attached' });
  assert.ok(Math.abs(await page.locator(`[data-message-id="${anchor.id}"]`).evaluate(el => el.getBoundingClientRect().top - document.querySelector('.ta-history').getBoundingClientRect().top) - anchor.offset) < 2, 'refresh never drags reader to bottom');
  await page.getByRole('button', { name: '回到最新 ↓' }).click();
  await input.fill('安排任务');
  await input.press('Shift+Enter');
  assert.equal(await input.inputValue(), '安排任务\n');
  await input.dispatchEvent('keydown', { key: 'Enter', isComposing: true });
  await input.dispatchEvent('keydown', { key: 'Enter', keyCode: 229 });
  await input.dispatchEvent('compositionstart');
  await input.press('Enter');
  assert.equal(await page.evaluate(() => window.calls.filter(([cmd]) => cmd === 'taskbar_assistant_send').length), 0, 'IME candidate confirmation is not send');
  await input.dispatchEvent('compositionend');
  await page.evaluate(() => { window.delaySend = true; });
  await input.press('Enter');
  await page.waitForFunction(() => typeof window.resolveSend === 'function');
  assert.equal(await input.inputValue(), '', 'sent text clears immediately, not after dispatch');
  await input.fill('下一条草稿');
  await input.press('Enter');
  assert.equal(await page.evaluate(() => window.calls.filter(([cmd]) => cmd === 'taskbar_assistant_send').length), 1, 'busy dispatch cannot duplicate');
  await page.evaluate(() => { window.delaySend = false; window.resolveSend(); });
  await page.waitForSelector('.ta-bubble:text-is("已安排")');
  assert.equal(await input.inputValue(), '下一条草稿', 'completed dispatch preserves newer draft');
  assert.equal(await page.evaluate(() => window.calls.some(([cmd]) => cmd === 'taskbar_bubble_reply')), false);
  await page.evaluate(() => { window.failSend = true; });
  await page.waitForFunction(() => !document.querySelector('.ta-submit').disabled);
  await input.press('Enter');
  await page.getByRole('alert').filter({ hasText: 'offline' }).waitFor();
  assert.equal(await input.inputValue(), '下一条草稿', 'failed dispatch preserves draft');
  // More than a page can arrive while hidden. Reopening must bridge the missing interval.
  await page.evaluate(() => {
    window.emit('taskbar-bubble:hidden');
    for (let i=0; i<65; i++) window.historyMessages.push({id:'gap'+i,role:'assistant',text:'补齐 '+i,createdAt:200+i});
    window.hover = true; window.emit('taskbar-bubble:open');
  });
  await page.waitForSelector('[data-message-id="gap0"]', { state: 'attached' });
  assert.equal(await page.locator('[data-message-id^="gap"]').count(), 65);
  assert.equal(await page.evaluate(() => { const ids = [...document.querySelectorAll('.ta-message')].map(el => el.dataset.messageId); return ids.length === new Set(ids).size; }), true);
  assert.equal(await input.inputValue(), '下一条草稿');
  // Any tab: the compose box stays visible and stray typing lands in it.
  await page.getByRole('tab', { name: '快捷', exact: true }).click();
  assert.equal(await input.isVisible(), true, 'assistant input is available on every tab');
  await page.keyboard.type('x');
  assert.equal(await input.inputValue(), '下一条草稿x', 'typing outside a field goes to the assistant');
  await page.getByRole('tab', { name: '会话', exact: true }).click();
  assert.equal(await page.locator('.ta-session').first().textContent(), '运行任务进行中最新一步');
  await page.locator('.ta-session').first().click();
  await page.getByRole('tab', { name: '助理', exact: true }).click();
  assert.equal(await page.evaluate(() => window.calls.some(([cmd, args]) => cmd === 'taskbar_bubble_open' && args.threadId === 'running')), true);

  for (const [width, height] of [[400, 340], [560, 200], [560, 640]]) {
    await page.setViewportSize({ width, height });
    const layout = await page.evaluate(() => {
      const panel = document.querySelector('.tb-panel').getBoundingClientRect();
      const input = document.querySelector('.ta-input').getBoundingClientRect();
      return { bottom: panel.bottom, width: document.querySelector('.tb-shell').offsetWidth, inputBottom: input.bottom, desired: window.calls.filter(([cmd]) => cmd === 'taskbar_bubble_place').map(([, args]) => args.height) };
    });
    assert.ok(layout.bottom <= height - 8 && layout.inputBottom < height, 'input remains inside a short viewport');
    assert.equal(layout.width, width);
    assert.ok(layout.desired.every(value => value === 640), 'placement never shrinks recursively');
  }
  assert.deepEqual(errors, []);
  console.log('Assistant lifecycle, focus, paging/anchor, dispatch, IME and constrained layout checks passed.');
} finally {
  await browser?.close();
  await server?.close();
}
