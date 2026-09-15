import assert from "node:assert/strict";
import { writeFile, rm, access } from "node:fs/promises";
import { spawn } from "node:child_process";
import { chromium } from "playwright-core";

// Real Solid/browser interaction with an in-memory disk at the IPC boundary.
const name = `workspace-panel-check-${process.pid}`;
let server, browser;
try {
  await writeFile(`${name}.html`, `<div id="root"></div><script type="module" src="/${name}.tsx"></script>`);
  await writeFile(`${name}.tsx`, `
import { render } from 'solid-js/web';
import { createSignal, Show } from 'solid-js';
import { api } from './src/ipc';
import { setState } from './src/store';
import WorkspacePanel from './src/components/WorkspacePanel';
import { Markdown } from './src/components/Markdown';
import './src/app.css';
let disk = '# Hello\\r\\n';
const files = new Map([['D:/demo/src/main.ts', 'const value = 1;\\n']]);
const reads = []; const directories = [];
window.testReads = () => reads;
window.testDirectories = () => directories;
const searches = [];
window.testSearches = () => searches;
api.searchWorkspaceFiles = async (_id, query) => { searches.push(query); return {entries: [{name:'hidden-result.md',path:'D:/demo/unexpanded/hidden-result.md',directory:false}],truncated:false}; };
window.testDisk = () => disk;
window.changeDisk = () => { disk = '# external\\r\\n'; };
api.previewWorkspaceFile = async (_id, path) => {
  path = path.startsWith('D:/') ? path : 'D:/demo/' + path;
  reads.push(path);
  if (path.endsWith('slow.md')) await new Promise(resolve => setTimeout(resolve, 250));
  const text = path.endsWith('README.md') ? disk : files.get(path) ?? '# slow';
  return { path, text, kind: path.endsWith('.ts') ? 'text' : 'markdown', size: text.length };
};
api.listWorkspaceDirectory = async (_id, path) => { directories.push(path); return { entries: path === 'src' ? [
 { name:'main.ts',path:'D:/demo/src/main.ts',directory:false },
] : [
 { name:'README.md',path:'README.md',directory:false },
 { name:'src',path:'src',directory:true },
 { name:'slow.md',path:'slow.md',directory:false },
 { name:'image.png',path:'image.png',directory:false },
], truncated:false }; };
api.saveWorkspaceFile = async (_id, _path, original, text) => { if (original !== disk) throw Error('文件已被外部修改'); disk = text; };
setState({ currentId:'check', cwd:'D:/demo', items:[{type:'assistant',id:1,ts:0,text:'[readme](README.md)'}] });
const [visible, setVisible] = createSignal(true);
const [fileRequest, setFileRequest] = createSignal(null);
window.addEventListener('nova:preview-file', event => { setFileRequest(event.detail); setVisible(true); });
render(() => <div style="display:flex;height:100vh"><main style="flex:1"><button onClick={() => setVisible(true)}>打开面板</button><Markdown text="[会话中的文件](src/main.ts)" markFiles /></main><Show when={visible()}><WorkspacePanel threadId="check" request={fileRequest()} onClose={() => setVisible(false)}/></Show></div>, document.getElementById('root')!);
`);
  const port = 14000 + process.pid % 1000;
  // Vite colors the banner even when piped, which splits the literal "Local:".
  server = spawn(process.execPath, ['node_modules/vite/bin/vite.js', '--host', '127.0.0.1', '--port', String(port)], { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'], env: { ...process.env, NO_COLOR: '1' } });
  await new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(Error('Vite startup timed out')), 20000);
    server.stdout.on('data', data => { if (data.toString().includes('Local:')) { clearTimeout(timeout); resolve(); } });
    server.on('exit', code => { clearTimeout(timeout); reject(Error('Vite exited: ' + code)); });
  });
  const candidates = [process.env.TEST_BROWSER, "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe", "C:/Program Files/Google/Chrome/Application/chrome.exe"].filter(Boolean);
  let executablePath;
  for (const path of candidates) { try { await access(path); executablePath = path; break; } catch {} }
  assert.ok(executablePath, "Set TEST_BROWSER to a Chromium executable");
  browser = await chromium.launch({ executablePath, headless: true });
  const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
  page.setDefaultTimeout(10000);
  const errors = [];
  page.on("pageerror", e => { errors.push(e.message); console.error(e.message); });
  await page.goto(`http://127.0.0.1:${port}/${name}.html`, { waitUntil: 'domcontentloaded', timeout: 30000 });
  await page.evaluate(() => document.documentElement.dataset.theme = 'ink-light');
  console.log('panel loaded');
  await page.getByRole('button', {name:'选择项目文件',exact:true}).waitFor();
  assert.equal(await page.locator('.workspace-picker').count(), 0, 'opening panel does not show picker');
  assert.deepEqual(await page.evaluate(() => window.testDirectories()), [], 'opening panel does not read directories');
  await page.getByRole('button', {name:'选择项目文件',exact:true}).click();
  await page.locator('.workspace-file').first().click();
  await page.getByRole('heading', {name:'Hello'}).waitFor();
  console.log('preview loaded');
  assert.equal(await page.locator('.workspace-picker').count(), 0, 'file picker collapses after selecting');
  assert.deepEqual(await page.evaluate(() => window.testDirectories()), [''], 'only load root initially');
  const bounds = await page.locator('.workspace-preview').boundingBox();
  const breadcrumb = await page.locator('.workspace-breadcrumb').boundingBox();
  const arrow = await page.locator('.workspace-breadcrumb > svg').last().boundingBox();
  assert.ok(arrow.y >= breadcrumb.y && arrow.y + arrow.height <= breadcrumb.y + breadcrumb.height, 'chevron stays in breadcrumb row');
  assert.equal(await page.locator('.workspace-preview .markdown').evaluate(el => getComputedStyle(el).fontSize), '15px');
  assert.ok(bounds.height > 650, 'content should occupy most of an 800px window');
  assert.equal(await page.getByRole('region', {name:'会话产物'}).getByRole('button', {name:'README.md'}).count(), 1, 'artifacts remain visible outside picker');
  await page.getByRole('button', {name:'选择项目文件',exact:true}).click();
  await page.getByRole('textbox', {name:'按文件名搜索项目'}).fill('hidden-result');
  await page.locator('.workspace-search-results').getByRole('button', {name:/hidden-result.md/}).waitFor();
  assert.deepEqual(await page.evaluate(() => window.testSearches()), ['hidden-result']);
  assert.deepEqual(await page.evaluate(() => window.testDirectories()), [''], 'search finds files without expanding directories');
  await page.getByRole('textbox', {name:'按文件名搜索项目'}).fill('');
  await page.getByRole('button', {name:'选择项目文件',exact:true}).click();
  await page.getByRole('button', {name:'编辑',exact:true}).click();
  await page.getByRole('textbox', {name:'文件内容编辑'}).fill('# Edited\n');
  await page.getByRole('textbox', {name:'文件内容编辑'}).press('Control+s');
  await page.getByRole('button', {name:'已保存',exact:true}).waitFor();
  console.log('saved');
  assert.equal(await page.evaluate(() => window.testDisk()), '# Edited\r\n', 'preserve CRLF');
  assert.equal(await page.getByRole('textbox', {name:'文件内容编辑'}).evaluate(el => el === document.activeElement), true, 'saving retains focus');
  await page.getByRole('textbox', {name:'文件内容编辑'}).fill('# Draft\n');
  await page.getByRole('button', {name:'预览',exact:true}).click();
  await page.getByRole('heading', {name:'Draft'}).waitFor();
  await page.getByRole('button', {name:'选择项目文件',exact:true}).click();
  await page.getByRole('treeitem', {name:'src',exact:true}).click();
  await page.getByRole('treeitem', {name:'main.ts',exact:true}).waitFor();
  assert.deepEqual(await page.evaluate(() => window.testDirectories()), ['', 'src'], 'expand loads only selected directory and reuses root cache');
  await page.getByRole('treeitem', {name:'main.ts',exact:true}).click();
  assert.equal(await page.getByRole('tab').count(), 2);
  await page.getByRole('button', {name:'编辑',exact:true}).click();
  await page.getByRole('textbox', {name:'文件内容编辑'}).fill('const changed = 2;\n');
  await page.getByRole('tab', {name:/README.md/}).click();
  await page.getByRole('heading', {name:'Draft'}).waitFor();
  await page.getByRole('tab', {name:/main.ts/}).click();
  assert.equal(await page.getByRole('textbox', {name:'文件内容编辑'}).inputValue(), 'const changed = 2;\n');
  const readsBefore = await page.evaluate(() => window.testReads().length);
  await page.getByRole('button', {name:'选择项目文件',exact:true}).click();
  await page.getByRole('treeitem', {name:'main.ts',exact:true}).click();
  assert.equal(await page.getByRole('tab').count(), 2, 'opening same file selects existing tab');
  assert.equal(await page.evaluate(() => window.testReads().length), readsBefore, 'tab switch does not reread disk');
  page.once('dialog', dialog => dialog.dismiss());
  await page.getByRole('button', {name:'关闭 main.ts',exact:true}).click();
  assert.equal(await page.getByRole('tab').count(), 2, 'cancel closing dirty tab retains it');
  await page.getByRole('button', {name:'选择项目文件',exact:true}).click();
  if (process.env.TEST_SCREENSHOT) await page.screenshot({ path: process.env.TEST_SCREENSHOT });
  await page.getByRole('treeitem', {name:'slow.md',exact:true}).click();
  await page.getByRole('tab', {name:/README.md/}).click();
  await page.waitForTimeout(350);
  assert.equal(await page.getByRole('tab').count(), 2, 'late file response cannot steal active tab');
  await page.getByRole('heading', {name:'Draft'}).waitFor();
  page.once('dialog', dialog => dialog.accept());
  await page.getByRole('button', {name:'关闭 main.ts',exact:true}).click();
  assert.equal(await page.getByRole('tab').count(), 1);
  await page.locator('.md-file-ref').click();
  await page.getByRole('tab', {name:'main.ts',exact:true}).waitFor();
  await page.getByRole('button', {name:'关闭 main.ts',exact:true}).click();
  await page.getByRole('button', {name:'关闭文件面板'}).click();
  await page.getByRole('button', {name:'打开面板'}).click();
  await page.getByRole('region', {name:'会话产物'}).getByRole('button', {name:'README.md'}).click();
  assert.equal(await page.getByRole('textbox', {name:'文件内容编辑'}).inputValue(), '# Draft\n');
  console.log('draft restored');
  await page.evaluate(() => window.changeDisk());
  await page.getByRole('button', {name:'保存',exact:true}).click();
  await page.getByRole('alert').filter({hasText:'外部修改'}).waitFor();
  console.log('conflict protected');
  assert.equal(await page.getByRole('textbox', {name:'文件内容编辑'}).inputValue(), '# Draft\n');
  assert.equal(await page.evaluate(() => window.testDisk()), '# external\r\n');
  await page.getByRole('button', {name:'选择项目文件',exact:true}).click();
  await page.locator('.workspace-tree .workspace-type-icon.folder').waitFor();
  await page.locator('.workspace-tree .workspace-type-icon.image').waitFor();
  await page.setViewportSize({width:700,height:800});
  const panel = await page.locator('.workspace-panel').boundingBox();
  assert.ok(panel.x >= 0 && panel.x + panel.width <= 701);
  assert.deepEqual(errors, []);
  console.log('Workspace panel: lazy tree, cached navigation, multi-tab drafts, duplicate open, close protection, stale requests, save and conflict passed');
} finally {
  await browser?.close();
  server?.kill();
  await Promise.all([`${name}.html`, `${name}.tsx`].map(path => rm(path, {force:true})));
}
