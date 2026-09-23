// Run: node scripts/workspace-explorer.test.mjs (installed Edge, or TEST_BROWSER executable).
import assert from 'node:assert/strict';
import { writeFile, rm } from 'node:fs/promises';
import { createServer } from 'vite';
import { chromium } from 'playwright-core';

const name = `workspace-explorer-check-${process.pid}`;
let server, browser;
try {
  await writeFile(`${name}.html`, `<div id="root"></div><script type="module" src="/${name}.tsx"></script>`);
  await writeFile(`${name}.tsx`, `
import { render } from 'solid-js/web';
import { createSignal } from 'solid-js';
import WorkspacePanel from './src/components/WorkspacePanel';
import { api } from './src/ipc';
import { setState } from './src/store';
import { setWorkspaceLayout } from './src/workspaceLayout';
import './src/app.css';
const file = {name:'readme.md',path:'/project/docs/readme.md',directory:false};
api.listWorkspaceDirectory = async (_,path) => ({entries:path ? [file] : [{name:'docs',path:'/project/docs',directory:true}],truncated:false});
api.searchWorkspaceFiles = async () => ({entries:[file],truncated:false});
api.previewWorkspaceFile = async (_, path) => path.endsWith('.zip')
  ? {path,kind:'external',text:null,size:78000}
  : {path:file.path,kind:'markdown',text:'# Explorer preview',size:18};
api.openInExplorer = async path => { window.revealedPath = path; };
const [request, setRequest] = createSignal(null);
window.openTestFile = path => setRequest({path});
setState({currentId:'explorer-check',cwd:'/project',items:[]});
setWorkspaceLayout({open:true,mode:'files',widthRatio:.65});
render(() => <div style="display:flex;height:100vh;width:100vw"><main style="flex:1;min-width:0">Chat</main><WorkspacePanel threadId="explorer-check" request={request()} onClose={() => {}} /></div>,document.getElementById('root')!);
`);
  server = await createServer({ server: { host: '127.0.0.1', port: 15000 + process.pid % 1000, strictPort: false } });
  await server.listen();
  browser = await chromium.launch({ ...(process.env.TEST_BROWSER ? { executablePath: process.env.TEST_BROWSER } : { channel: 'msedge' }), headless: true });
  const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.goto(`${server.resolvedUrls.local[0]}${name}.html`);
  const tree = page.getByRole('tree', { name: '项目文件树' });
  await tree.getByRole('treeitem', { name: 'docs', exact: true }).click();
  await tree.getByRole('treeitem', { name: 'readme.md' }).click();
  await page.getByRole('heading', { name: 'Explorer preview' }).waitFor();
  assert.equal(await tree.isVisible(), true, 'opening a file keeps the directory visible');
  assert.equal(await tree.getByRole('treeitem', { name: 'readme.md' }).getAttribute('aria-selected'), 'true');
  await page.getByRole('heading', { name: 'Explorer preview' }).click();
  assert.equal(await tree.isVisible(), true, 'clicking the preview must not dismiss navigation');
  const picker = page.getByLabel('项目目录', { exact: true });
  const content = page.locator('.workspace-file-content');
  for (const width of [1280, 700, 360]) {
    await page.setViewportSize({ width, height: 800 });
    const left = await picker.boundingBox(), right = await content.boundingBox();
    assert.ok(left.x + left.width <= right.x && right.width > 100, 'panes must not overlap');
    assert.ok(left.height > 500 && right.height > 500, 'both panes fill the available height');
  }
  await page.setViewportSize({ width: 1280, height: 800 });
  const separator = page.getByRole('separator', { name: '调整目录栏宽度' });
  await separator.focus();
  await page.keyboard.press('ArrowRight');
  assert.equal(await separator.getAttribute('aria-valuenow'), '40');
  const before = await picker.boundingBox(), grip = await separator.boundingBox();
  await page.mouse.move(grip.x + grip.width / 2, grip.y + 100);
  await page.mouse.down();
  await page.mouse.move(grip.x + 60, grip.y + 100);
  await page.mouse.up();
  assert.ok((await picker.boundingBox()).width > before.width, 'dragging resizes the directory pane');
  await page.getByRole('textbox', { name: '按文件名搜索项目' }).fill('readme');
  const result = page.getByLabel('文件搜索结果').getByRole('button', { name: /readme.md/ });
  await result.click();
  assert.equal(await result.getAttribute('aria-pressed'), 'true');
  await page.getByRole('textbox', { name: '按文件名搜索项目' }).fill('');
  await page.getByRole('button', { name: '折叠全部目录' }).click();
  assert.equal(await tree.getByRole('treeitem').count(), 1);
  await page.getByRole('button', { name: '切换目录栏' }).click();
  assert.equal(await picker.count(), 0);
  await page.getByRole('button', { name: '打开文件', exact: true }).click();
  assert.equal(await picker.isVisible(), true);
  if (process.env.TEST_SCREENSHOT) {
    await tree.getByRole('treeitem', { name: 'docs', exact: true }).click();
    await page.screenshot({ path: process.env.TEST_SCREENSHOT });
  }
  await page.getByRole('button', { name: '关闭 readme.md', exact: true }).click();
  await page.getByText('选择文件以查看预览', { exact: true }).waitFor();
  assert.equal(await picker.isVisible(), true, 'closing the last file keeps navigation available');
  // Keep every directory in the canonical Windows path, including files outside cwd.
  const zip = String.raw`\\?\D:\code\autotest\dist\webtest-20260922-132409\webtest-source.zip`;
  await page.evaluate(path => window.openTestFile(path), zip);
  const location = page.getByRole('button', { name: '切换目录栏' });
  await page.getByText('此文件类型或大小不适合内嵌预览', { exact: false }).waitFor();
  assert.equal(await location.getAttribute('title'), zip);
  assert.equal(await page.locator('.workspace-breadcrumb-text').textContent(), zip);
  for (const target of [location, page.getByRole('tab', { name: 'webtest-source.zip' }), page.locator('.workspace-preview > p')]) {
    await page.evaluate(() => { window.revealedPath = null; });
    await target.click({ button: 'right' });
    await page.getByRole('button', { name: '打开所在目录', exact: true }).click();
    assert.equal(await page.evaluate(() => window.revealedPath), zip, 'reveal uses the full file path');
  }
  await page.evaluate(() => window.openTestFile('/project/docs/readme.md'));
  await page.getByRole('heading', { name: 'Explorer preview' }).waitFor();
  await page.getByRole('tab', { name: 'webtest-source.zip' }).click({ button: 'right' });
  await page.getByRole('button', { name: '打开所在目录', exact: true }).click();
  assert.equal(await page.evaluate(() => window.revealedPath), zip, 'inactive tabs reveal their own file');
  assert.deepEqual(errors, []);
  console.log('Workspace explorer: persistent navigation, selection, search, collapse, pointer/keyboard resize and narrow layouts passed');
} finally {
  await browser?.close();
  await server?.close();
  await Promise.all([`${name}.html`, `${name}.tsx`].map(path => rm(path, { force: true })));
}
