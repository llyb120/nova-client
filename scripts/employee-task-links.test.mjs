// Isolated employee board: no real IPC, tasks, sessions or user browser state.
import assert from 'node:assert/strict';
import { access, readFile } from 'node:fs/promises';
import { build } from 'esbuild';
import { transformAsync } from '@babel/core';
import solid from '@solidjs/babel-plugin';
import { chromium } from 'playwright-core';

const task = (id, text, extra = {}) => ({ id, text, confirm: false, createdAt: 1, ...extra });
const source = task('t1', '重复任务', { status: 'stopped', finishedAt: 9999, result: [
  '已有任务 `t2` 正在执行，因此停止当前任务。原记录 t100，旧版步骤 t101。',
  '**t2**、t9999、xt2、t2x、t2/child、/tmp/t2 保持原文。',
  '[t2](https://example.org/t2)',
  '```text\nt2\n```',
].join('\n\n') });
const snapshot = {
  employee: { enabled: true, duties: [], inbox: [task('t2', '正在执行的目标任务')], runs: [],
    finished: [source, ...Array.from({ length: 40 }, (_, i) => task(`t${i + 10}`, `历史任务 ${i}`, { status: 'done', finishedAt: i + 10 })),
      task('t100', '更早的目标记录', { status: 'done', finishedAt: 1 }),
      task('t101', '旧版目标步骤', { parent: 't100', status: 'failed', finishedAt: 1 })] },
  status: 'duty', tasks: [], parallel: 4, nextCheckAt: 0, dues: {}, idleSupported: true,
};
const compiled = await transformAsync(`import {render} from '@solidjs/web';
  import EmployeeView from './src/components/EmployeeView';
  window.snapshot = ${JSON.stringify(snapshot)}; window.calls = []; window.urls = [];
  render(() => <EmployeeView />, document.body);`, {
  filename: 'employee-links-check.tsx', configFile: false, babelrc: false,
  plugins: [[solid, { generate: 'dom' }]], parserOpts: { plugins: ['jsx', 'typescript'] },
});
const mocks = {
  store: 'export const state={settings:{relayToken:""},peers:[],currentId:null}; export const openThread=()=>{throw Error("unexpected session navigation")}; export const showToast=text=>window.toast=text;',
  ipc: 'export const api={openUrl:async url=>window.urls.push(url)}; export const fileUriPath=s=>s;',
  TitleBar: 'export const TitleBarPortal=props=>props.children;',
  VaultPop: 'export default ()=>null;',
  FileContextMenu: 'export const createFileContextMenu=()=>({open(){},Menu:()=>null});',
  '@tauri-apps/api/core': 'export const convertFileSrc=s=>s; export const invoke=async (command,args)=>{window.calls.push({command,args}); if(command==="employee_get")return structuredClone(window.snapshot); throw Error("unexpected IPC: "+command);};',
  '@tauri-apps/api/event': 'export const listen=async (name,cb)=>{window.employeeChanged=cb;return ()=>{};};',
  '@tauri-apps/plugin-dialog': 'export const confirm=async()=>false; export const message=async()=>{};',
};
const bundle = await build({
  stdin: { contents: compiled.code, resolveDir: process.cwd(), sourcefile: 'employee-links-check.tsx', loader: 'ts' },
  bundle: true, write: false, format: 'esm', platform: 'browser', conditions: ['browser'], outdir: '/tmp/employee-links-check',
  plugins: [{ name: 'employee-fixture', setup(builder) {
    builder.onResolve({ filter: /^\.\.?\/(?:.*\/)?(store|ipc|TitleBar|VaultPop|FileContextMenu)$|^@tauri-apps\// }, ({ path }) => ({ path: path.startsWith('@') ? path : path.split('/').at(-1), namespace: 'mock' }));
    builder.onLoad({ filter: /.*/, namespace: 'mock' }, ({ path }) => ({ contents: mocks[path], loader: 'js' }));
    builder.onLoad({ filter: /\.tsx$/ }, async ({ path }) => {
      const result = await transformAsync(await readFile(path, 'utf8'), { filename: path, configFile: false, babelrc: false,
        plugins: [[solid, { generate: 'dom' }]], parserOpts: { plugins: ['jsx', 'typescript'] } });
      return { contents: result.code, loader: 'ts' };
    });
  } }],
});
let executablePath;
for (const candidate of [process.env.TEST_BROWSER, chromium.executablePath(),
  `${process.env['ProgramFiles(x86)']}/Microsoft/Edge/Application/msedge.exe`, '/usr/bin/chromium'].filter(Boolean)) {
  try { await access(candidate); executablePath = candidate; break; } catch {}
}
assert.ok(executablePath, 'Set TEST_BROWSER to an installed Chromium');
const browser = await chromium.launch({ headless: true, executablePath, args: ['--no-sandbox'] });
try {
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.setContent('<style>:root{--accent:#06c}body{margin:0;height:900px;display:flex}</style>');
  await page.addStyleTag({ content: bundle.outputFiles.find(file => file.path.endsWith('.css')).text });
  await page.addScriptTag({ type: 'module', content: bundle.outputFiles.find(file => file.path.endsWith('.js')).text });
  const heading = page.locator('.employee-detail h2');
  await heading.filter({ hasText: '正在执行的目标任务' }).waitFor();
  const history = page.locator('.employee-nav button').filter({ hasText: '记录' });
  await history.click();
  await page.getByRole('button', { name: '已停止', exact: true }).click();
  const result = page.locator('.employee-transcript-result');
  await result.locator('code a[data-employee-task="t2"]').waitFor();
  assert.equal(await result.locator('a[data-employee-task="t2"]').count(), 2, 'plain and inline-code mentions become links, not paths or longer identifiers');
  assert.equal(await result.locator('a[data-employee-task="t9999"]').count(), 0, 'unknown task IDs stay text');
  assert.equal(await result.locator('pre a').count(), 0, 'code blocks remain unchanged');
  assert.equal(await result.locator('a[href="https://example.org/t2"]').count(), 1, 'existing Markdown links remain unchanged');
  assert.equal(await result.locator('strong a[data-employee-task="t2"]').count(), 1, 'Markdown formatting is retained');
  await result.locator('code a[data-employee-task="t2"]').press('Enter');
  await heading.filter({ hasText: '正在执行的目标任务' }).waitFor();
  assert.equal(await page.locator('.employee-nav button[aria-pressed="true"]').textContent(), '任务 1');
  assert.equal(await page.locator('.employee-detail').evaluate(el => el === document.activeElement), true, 'keyboard focus follows the jump');
  assert.equal(await page.evaluate(() => location.hash), '', 'internal jumps do not navigate the WebView');

  await history.click();
  await page.getByRole('button', { name: '已停止', exact: true }).click();
  await result.locator('a[data-employee-task="t100"]').click();
  await heading.filter({ hasText: '更早的目标记录' }).waitFor();
  assert.equal(await page.getByRole('button', { name: '全部', exact: true }).getAttribute('aria-pressed'), 'true', 'hidden history filters are cleared');
  assert.ok(await page.locator('.employee-row').count() > 30, 'pagination expands to include the target');
  assert.match(await page.locator('.employee-row[aria-current="true"]').textContent(), /更早的目标记录/);

  const pickSource = () => page.locator('.employee-row').filter({ hasText: '重复任务' }).click();
  await pickSource();
  await result.locator('a[data-employee-task="t101"]').click();
  await page.locator('.employee-tree-detail .employee-duty-body').filter({ hasText: '旧版目标步骤' }).waitFor();
  assert.match(await page.locator('.employee-row[aria-current="true"]').textContent(), /更早的目标记录/, 'legacy child IDs select their own detail and highlight the owning row');

  await pickSource();
  await page.evaluate(() => {
    window.snapshot.employee.inbox.push({ id: 't3', text: '新目标', confirm: false, createdAt: 1 });
    window.snapshot.employee.finished.find(task => task.id === 't1').result += '\n\n新增目标 `t3`';
    window.employeeChanged();
  });
  await result.locator('a[data-employee-task="t3"]').waitFor();
  await page.evaluate(() => {
    window.snapshot.employee.inbox = window.snapshot.employee.inbox.filter(task => task.id !== 't2');
    window.employeeChanged();
  });
  await result.locator('a[data-employee-task="t2"]').waitFor({ state: 'detached' });
  assert.match(await result.locator('code').first().textContent(), /t2/, 'removed task links degrade to their original text');
  await page.evaluate(() => {
    window.snapshot.employee.finished.find(task => task.id === 't1').result = '更新结果：`t100`';
    window.employeeChanged();
  });
  await result.locator('code a[data-employee-task="t100"]').waitFor();
  assert.equal(await result.locator('a[data-employee-task]').count(), 1, 'result updates are linked without duplicate anchors');
  assert.deepEqual(await page.evaluate(() => window.urls), []);
  assert.ok((await page.evaluate(() => window.calls)).every(call => call.command === 'employee_get'), 'jumps never restart or modify tasks');
  assert.deepEqual(errors, []);
  console.log('Employee task ID links: passed');
} finally {
  await browser.close();
}
