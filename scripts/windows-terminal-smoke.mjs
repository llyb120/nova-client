// Windows-only end-to-end test. Build the debug app first; no IPC or PTY mocks.
// Usage: node scripts/windows-terminal-smoke.mjs [src-tauri/target/debug/nova.exe]
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { mkdtemp, mkdir, rm, writeFile, access } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { randomUUID } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';
import { createServer } from 'vite';
import { chromium } from 'playwright-core';

if (process.platform !== 'win32') throw new Error('This test requires Windows and WebView2.');
const executable = path.resolve(process.argv[2] || 'src-tauri/target/debug/nova.exe');
await access(executable);
const root = await mkdtemp(path.join(tmpdir(), 'nova-terminal-smoke-'));
const project = path.join(root, 'project with spaces 中文');
await mkdir(project);
const marker = `nova-native-${randomUUID()}`;
const port = Number(process.env.TEST_CDP_PORT || 9222);
const report = [];
const appLogs = [];
let server, app, browser, page;
const bounded = (promise, message, milliseconds = 45000) => {
  let timer;
  return Promise.race([promise, new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(message)), milliseconds);
  })]).finally(() => clearTimeout(timer));
};
try {
  server = await createServer({ server: { host: '127.0.0.1', port: 5173, strictPort: true } });
  await server.listen();
  app = spawn(executable, [], { windowsHide: false, env: {
    ...process.env, NOVA_DATA_DIR: path.join(root, 'data'), NOVA_TERMINAL_SMOKE_MARKER: marker,
    WEBVIEW2_USER_DATA_FOLDER: path.join(root, 'webview'),
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}`,
  }, stdio: ['ignore', 'pipe', 'pipe'] });
  app.on('error', error => appLogs.push(String(error)));
  for (const stream of [app.stdout, app.stderr]) stream.on('data', data => appLogs.push(data.toString()));
  for (let attempt = 0; attempt < 180; attempt++) {
    if (app.exitCode !== null) throw new Error(`Nova exited: ${app.exitCode}\n${appLogs.join('')}`);
    try {
      const response = await fetch(`http://127.0.0.1:${port}/json/version`, { signal: AbortSignal.timeout(1000) });
      if (response.ok) { browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`); break; }
    } catch { /* WebView2 is still starting. */ }
    await delay(500);
  }
  assert.ok(browser, 'WebView2 did not start its debugging endpoint');
  const context = browser.contexts()[0];
  page = context.pages()[0] || await context.waitForEvent('page');
  page.setDefaultTimeout(30000);
  page.on('pageerror', error => appLogs.push(`WebView: ${error.stack || error}`));
  await page.waitForFunction(() => !!window.__TAURI__?.core, null, { timeout: 60000 });
  await bounded(page.evaluate(async cwd => {
    const sessions = await import('/src/terminalSessions.ts');
    const store = await import('/src/store.ts');
    const layout = await import('/src/workspaceLayout.ts');
    layout.setHomeTerminalCwd(cwd);
    window.nativeTerminalSmoke = {
      sessions, store, layout,
      group: () => sessions.getTerminalGroup('home'),
      active: () => { const group = sessions.getTerminalGroup('home'); return group.tabs().find(t => t.id === group.activeId()); },
      buffer: () => {
        const tab = window.nativeTerminalSmoke.active();
        if (!tab) return '';
        const buffer = tab.terminal.buffer.active;
        return Array.from({ length: buffer.length }, (_, i) => buffer.getLine(i)?.translateToString(true) || '').join('\n');
      },
    };
  }, project), 'Loading terminal modules timed out');
  await page.waitForFunction(() => !!window.nativeTerminalSmoke.store.state.settings, null, { timeout: 60000 });
  const shells = [
    { shell: '', args: [], command: 'echo %NOVA_TERMINAL_SMOKE_MARKER%', name: 'default-cmd' },
    { shell: 'powershell.exe', args: ['-NoLogo', '-NoProfile'], command: 'Write-Output $env:NOVA_TERMINAL_SMOKE_MARKER', name: 'powershell' },
    { shell: 'pwsh.exe', args: ['-NoLogo', '-NoProfile'], command: 'Write-Output $env:NOVA_TERMINAL_SMOKE_MARKER', name: 'pwsh' },
  ];
  for (const shell of shells) {
    await bounded(page.evaluate(async config => {
      const settings = await window.__TAURI__.core.invoke('get_settings');
      settings.terminalShell = config.shell;
      settings.terminalArgs = config.args;
      await window.__TAURI__.core.invoke('set_settings', { settings });
      window.nativeTerminalSmoke.store.setState('settings', settings);
      window.nativeTerminalSmoke.layout.setWorkspaceLayout({ open: false, mode: 'terminal' });
    }, shell), `${shell.name}: saving settings timed out`);
    await page.keyboard.press('Control+Backquote');
    await page.getByRole('region', { name: '交互式终端' }).waitFor();
    await page.waitForFunction(() => ['running', 'error', 'exited'].includes(window.nativeTerminalSmoke.active()?.status()));
    assert.equal(await page.evaluate(() => window.nativeTerminalSmoke.active().status()), 'running',
      `${shell.name}: ${await page.evaluate(() => window.nativeTerminalSmoke.active()?.error())}`);
    const id = await page.evaluate(() => window.nativeTerminalSmoke.active().id);
    await page.evaluate(command => window.nativeTerminalSmoke.active().terminal.paste(command + '\r'), shell.command);
    await page.waitForFunction(text => window.nativeTerminalSmoke.buffer().includes(text), marker);
    await page.keyboard.press('Control+Backquote');
    await page.getByRole('region', { name: '交互式终端' }).waitFor({ state: 'hidden' });
    await page.keyboard.press('Control+Backquote');
    await page.getByRole('region', { name: '交互式终端' }).waitFor();
    assert.equal(await page.evaluate(() => window.nativeTerminalSmoke.active().id), id);
    await page.getByRole('button', { name: '新建终端', exact: true }).click();
    await page.waitForFunction(() => window.nativeTerminalSmoke.group().tabs().length === 2 && window.nativeTerminalSmoke.active()?.status() === 'running');
    await page.getByRole('tab', { name: /终端 1/ }).click();
    assert.equal(await page.evaluate(() => window.nativeTerminalSmoke.active().id), id);
    assert.ok((await page.evaluate(() => window.nativeTerminalSmoke.buffer())).includes(marker));
    await page.screenshot({ path: `windows-terminal-${shell.name}.png` });
    await page.evaluate(() => window.nativeTerminalSmoke.active().terminal.paste('exit\r'));
    await page.waitForFunction(() => window.nativeTerminalSmoke.active()?.status() === 'exited');
    await bounded(page.evaluate(async () => {
      const test = window.nativeTerminalSmoke, group = test.group();
      for (const tab of [...group.tabs()]) await test.sessions.closeTerminalTab(group, tab);
    }), `${shell.name}: closing terminals timed out`);
    report.push({ shell: shell.name, result: 'passed', cwd: project });
    console.log(`${shell.name}: real WebView2 + IPC + PTY, output, two tabs, hide/restore, exit and close passed`);
  }
} catch (error) {
  report.push({ result: 'failed', error: String(error.stack || error) });
  if (page) {
    await page.screenshot({ path: 'windows-terminal-failure.png' }).catch(() => {});
    console.error(await page.evaluate(() => ({ url: location.href, body: document.body.innerText,
      status: window.nativeTerminalSmoke?.active()?.status(), error: window.nativeTerminalSmoke?.active()?.error(),
      buffer: window.nativeTerminalSmoke?.buffer() })).catch(() => ({})));
  }
  throw error;
} finally {
  await writeFile('windows-terminal-smoke.json', JSON.stringify(report, null, 2));
  await writeFile('windows-terminal-app.log', appLogs.join('\n'));
  await browser?.close().catch(() => {});
  if (app?.pid && app.exitCode === null) spawnSync('taskkill.exe', ['/PID', String(app.pid), '/T', '/F'], { windowsHide: true });
  await server?.close();
  await rm(root, { recursive: true, force: true, maxRetries: 3 }).catch(() => {});
}
