import assert from 'node:assert/strict';
import { access, rm, writeFile } from 'node:fs/promises';
import { spawn } from 'node:child_process';
import { chromium } from 'playwright-core';

const name = `antigravity-account-transfer-${process.pid}`;
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

const initialAccounts = [
  { id: 'acc-1', label: '主号', email: 'alice@example.test', authMethod: 'oauth-personal', appDataDir: 'antigravity-acp-1', tierName: 'Pro', tierId: null, addedAt: 1, lastUsedAt: 2 },
  { id: 'acc-2', label: '备用号', email: 'bob@example.test', authMethod: 'oauth-personal', appDataDir: 'antigravity-acp-2', tierName: null, tierId: null, addedAt: 1, lastUsedAt: 1 },
];
let currentSnap = {
  accounts: initialAccounts.map(a => ({ ...a })),
  activeAccountId: 'acc-1',
  currentEmail: 'alice@example.test',
  currentAuthMethod: 'oauth-personal',
  credentialSupported: true,
  canInteractiveLogin: true,
};
let exportedCall = null;
let importedCall = null;

api.listAntigravityAccounts = async () => structuredClone(currentSnap);
api.getModelOptions = async () => ({ configOptions: [], modes: null });
api.exportAntigravityAccounts = async (accountIds, path) => {
  exportedCall = { accountIds: [...accountIds], path };
  return { exportedCount: accountIds.length, skippedCount: 0, warnings: [], path };
};
api.importAntigravityAccounts = async (path) => {
  importedCall = { path };
  const carol = {
    id: 'acc-3',
    label: '新导入号',
    email: 'carol@example.test',
    authMethod: 'oauth-personal',
    appDataDir: 'antigravity-acp-3',
    tierName: 'Ultra',
    tierId: null,
    addedAt: 3,
    lastUsedAt: 0,
  };
  currentSnap = {
    ...currentSnap,
    accounts: [...currentSnap.accounts, carol],
  };
  return {
    snapshot: structuredClone(currentSnap),
    importedCount: 1,
    updatedCount: 0,
    skippedCount: 0,
    warnings: [],
    activeUpdated: false,
  };
};

flush(() => setState(s => {
  s.settings = { simpleMode: false, antigravityEnabled: true, antigravityPath: 'agy', terminalMenuItems: [] };
  s.backendAvailability = { antigravity: true };
}));

window.fixture = {
  exportedCall: () => exportedCall,
  importedCall: () => importedCall,
};

const [open, setOpen] = createSignal(true);
render(() => <Show when={open()}><SettingsModal initialTab="backends" onClose={() => setOpen(false)} /></Show>, document.getElementById('root')!);
`);

  const port = 18500 + (process.pid % 1000);
  server = spawn(process.execPath, ['node_modules/vite/bin/vite.js', '--host', '127.0.0.1', '--port', String(port), '--strictPort'], {
    windowsHide: true,
    stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, NO_COLOR: '1' },
  });
  await new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error('Vite startup timed out')), 30000);
    server.stdout.on('data', data => { if (data.toString().includes('Local:')) { clearTimeout(timeout); resolve(); } });
    server.stderr.on('data', data => process.stderr.write(data));
    server.once('error', error => { clearTimeout(timeout); reject(error); });
    server.once('exit', code => { clearTimeout(timeout); reject(new Error('Vite exited: ' + code)); });
  });

  let executablePath;
  for (const path of [process.env.TEST_BROWSER, '/usr/bin/google-chrome', '/usr/bin/chromium', 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe'].filter(Boolean)) {
    try { await access(path); executablePath = path; break; } catch { /* next candidate */ }
  }
  browser = await chromium.launch({ executablePath, headless: true, args: ['--no-sandbox'] });
  const page = await browser.newPage({ viewport: { width: 1100, height: 850 } });
  page.setDefaultTimeout(15000);
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));

  await page.addInitScript(() => {
    let callback = 0;
    window.__TAURI_INTERNALS__ = {
      transformCallback: () => ++callback,
      metadata: { currentWindow: { label: 'main' }, currentWebview: { label: 'main' } },
      invoke: async (command) => {
        if (command === 'plugin:app|version') return '0.0.0';
        if (command === 'plugin:dialog|save') return 'C:/tmp/exported-accounts.json';
        if (command === 'plugin:dialog|open') return 'C:/tmp/incoming-accounts.json';
        throw new Error(`Unexpected IPC: ${command}`);
      },
    };
  });

  await page.goto(`http://127.0.0.1:${port}/${name}.html`, { waitUntil: 'domcontentloaded' });
  await page.getByText('alice@example.test').waitFor({ state: 'visible' });
  await page.getByText('bob@example.test').waitFor({ state: 'visible' });

  // 1. 进入多选导出模式，默认全选，取消勾选 bob 后导出
  await page.getByRole('button', { name: '导出账号' }).click();
  const aliceCheckbox = page.getByLabel('选择导出 主号');
  const bobCheckbox = page.getByLabel('选择导出 备用号');
  await aliceCheckbox.waitFor({ state: 'visible' });
  assert.equal(await aliceCheckbox.isChecked(), true);
  assert.equal(await bobCheckbox.isChecked(), true);

  await bobCheckbox.uncheck();
  assert.equal(await bobCheckbox.isChecked(), false);
  await page.getByRole('button', { name: '导出所选 (1)' }).click();

  await page.waitForFunction(() => !!window.fixture.exportedCall());
  const exported = await page.evaluate(() => window.fixture.exportedCall());
  assert.deepEqual(exported, {
    accountIds: ['acc-1'],
    path: 'C:/tmp/exported-accounts.json',
  });
  await aliceCheckbox.waitFor({ state: 'hidden' });
  await page.getByText(/已导出 1 个账号至 C:\/tmp\/exported-accounts\.json/).waitFor({ state: 'visible' });

  // 2. 导入账号后列表立即可见且提示完整
  await page.getByRole('button', { name: '导入账号' }).click();
  await page.waitForFunction(() => !!window.fixture.importedCall());
  const imported = await page.evaluate(() => window.fixture.importedCall());
  assert.deepEqual(imported, { path: 'C:/tmp/incoming-accounts.json' });
  await page.getByText('carol@example.test').waitFor({ state: 'visible' });
  await page.getByText(/导入完成：新增 1 个/).waitFor({ state: 'visible' });

  assert.deepEqual(errors, []);
  console.log('Antigravity account transfer: selective multi-account export and import UI verified');
} finally {
  await browser?.close();
  server?.kill();
  await Promise.all([`${name}.html`, `${name}.tsx`].map(path => rm(path, { force: true })));
}
