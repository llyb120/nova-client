import assert from 'node:assert/strict';
import { access, rm, writeFile } from 'node:fs/promises';
import { spawn } from 'node:child_process';
import { chromium } from 'playwright-core';

// Real settings form, synthetic account metadata and a mocked settings save.
const name = `terminal-account-settings-${process.pid}`;
let server, browser;
try {
  await writeFile(`${name}.html`, `<div id="root"></div><script type="module" src="/${name}.tsx"></script>`);
  await writeFile(`${name}.tsx`, `
import { render } from '@solidjs/web';
import { createSignal, flush, Show } from 'solid-js';
import { SettingsModal } from './src/components/SettingsModal';
import { api } from './src/ipc';
import { setState } from './src/store';
import './src/app.css';
const accounts = ['alice', 'bob'].map(id => ({ id, label:id, email:id+'@example.test', authMethod:'oauth-personal' }));
const snap = ids => ({ accounts:accounts.filter(a => ids.includes(a.id)).map(a => ({...a})), activeAccountId:'alice', credentialSupported:true });
const [open, setOpen] = createSignal(true);
let loadAccounts;
let saved;
let switches = 0;
api.listAntigravityAccounts = () => new Promise(resolve => { loadAccounts = resolve; });
api.getModelOptions = async () => ({ configOptions:[], modes:null });
api.setSettings = async settings => { saved = structuredClone(settings.terminalMenuItems); };
api.switchAntigravityAccount = async () => { switches++; };
flush(() => setState(s => { s.settings = { simpleMode:true, terminalMenuItems:[
  { id:'first', name:'AGY 1', mode:'antigravity', agyAccountId:'alice', command:'', shell:'', args:[] },
  { id:'second', name:'AGY 2', mode:'antigravity', command:'', shell:'', args:[] },
] }; }));
window.fixture = {
  loaded: () => !!loadAccounts,
  resolveAccounts: () => loadAccounts(snap(['alice', 'bob'])),
  refresh: ids => flush(() => setState(s => { s.antigravityAccounts = snap(ids); })),
  reopen: () => setOpen(true),
  saved: () => saved,
  switches: () => switches,
};
render(() => <Show when={open()}><SettingsModal onClose={() => setOpen(false)}/></Show>, document.getElementById('root')!);
`);
  const port = 18000 + process.pid % 1000;
  server = spawn(process.execPath, ['node_modules/vite/bin/vite.js', '--host', '127.0.0.1', '--port', String(port), '--strictPort'], {
    windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'], env: { ...process.env, NO_COLOR: '1' },
  });
  await new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error('Vite startup timed out')), 30000);
    server.stdout.on('data', data => { if (data.toString().includes('Local:')) { clearTimeout(timeout); resolve(); } });
    server.stderr.on('data', data => process.stderr.write(data));
    server.once('error', error => { clearTimeout(timeout); reject(error); });
    server.once('exit', code => { clearTimeout(timeout); reject(new Error('Vite exited: '+code)); });
  });
  let executablePath;
  for (const path of [process.env.TEST_BROWSER, '/usr/bin/google-chrome', '/usr/bin/chromium', 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe'].filter(Boolean)) {
    try { await access(path); executablePath = path; break; } catch { /* next candidate */ }
  }
  browser = await chromium.launch({ executablePath, headless:true, args:['--no-sandbox'] });
  const page = await browser.newPage({ viewport:{ width:1100, height:850 } });
  page.setDefaultTimeout(15000);
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.addInitScript(() => {
    let callback = 0;
    window.__TAURI_INTERNALS__ = {
      transformCallback: () => ++callback,
      metadata: { currentWindow:{ label:'main' }, currentWebview:{ label:'main' } },
      invoke: async command => {
        if (command === 'plugin:app|version') return '0.0.0';
        throw new Error(`Unexpected IPC: ${command}`);
      },
    };
  });
  await page.goto(`http://127.0.0.1:${port}/${name}.html`, { waitUntil:'domcontentloaded' });
  const fields = page.getByLabel('反重力 CLI 登录账号', { exact:true });
  const first = fields.first(), second = fields.nth(1);
  await page.waitForFunction(() => window.fixture?.loaded());
  assert.equal(await first.inputValue(), 'alice');
  await page.evaluate(() => window.fixture.resolveAccounts());
  await first.locator('option', { hasText:'bob@example.test' }).waitFor({ state:'attached' });
  assert.equal(await first.inputValue(), 'alice', 'keep the saved choice when accounts finish loading');
  await first.selectOption('bob');
  await second.selectOption('alice');
  assert.equal(await first.inputValue(), 'bob');
  assert.equal(await second.inputValue(), 'alice');
  await page.evaluate(() => window.fixture.refresh(['alice']));
  assert.equal(await first.inputValue(), 'bob', 'missing account must not reset to current login');
  await page.evaluate(() => window.fixture.refresh(['alice', 'bob']));
  assert.equal(await first.inputValue(), 'bob', 'account refresh must not reset the selection');
  await second.selectOption('');
  await page.getByRole('button', { name:'保存', exact:true }).click();
  await page.waitForFunction(() => !!window.fixture.saved());
  const saved = await page.evaluate(() => window.fixture.saved());
  assert.equal(saved[0].agyAccountId, 'bob');
  assert.equal(saved[1].agyAccountId, undefined);
  await fields.first().waitFor({ state:'hidden' });
  await page.evaluate(() => window.fixture.reopen());
  assert.equal(await first.inputValue(), 'bob');
  assert.equal(await second.inputValue(), '');
  assert.equal(await page.evaluate(() => window.fixture.switches()), 0, 'editing terminal presets must not switch global accounts');
  assert.deepEqual(errors, []);
  console.log('Terminal account settings: selection survives refresh and save without switching the global login');
} finally {
  await browser?.close();
  server?.kill();
  await Promise.all([`${name}.html`, `${name}.tsx`].map(path => rm(path, { force:true })));
}
