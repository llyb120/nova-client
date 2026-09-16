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
import { EditorView } from '@codemirror/view';
import AppearanceLayoutSettings from './src/components/AppearanceLayoutSettings';
import { workspaceLayout } from './src/workspaceLayout';
let windowLayout = {width:1280,height:820,maximized:false,remember:true};
api.getWindowLayout = async () => ({...windowLayout});
api.setWindowLayout = async patch => { windowLayout = patch.reset ? {width:1280,height:820,maximized:false,remember:true} : {...windowLayout,...patch}; };
window.showLayoutSettings = () => {
  const host = document.createElement('div');
  host.className = 'settings-modal'; host.style.cssText = 'position:fixed;inset:20px 20px 20px auto;width:500px;overflow:auto;z-index:1000;padding:20px;background:var(--bg-panel)';
  document.body.append(host);
  const dispose = render(() => <AppearanceLayoutSettings />, host);
  window.hideLayoutSettings = () => { dispose(); host.remove(); };
};
window.testLayout = () => ({...workspaceLayout});
window.testCode = () => EditorView.findFromDOM(document.querySelector('.cm-editor'));
window.loadHistory = () => setState('items', [{type:'tool',id:2,ts:0,status:'completed',kind:'edit',locations:[{path:'generated.ts'}],content:[]}]);
api.workspaceGitStatus = async () => ({repo:'D:/demo',files:[{path:'src/main.ts',oldPath:null,index:'M',worktree:'M'},{path:'new.ts',oldPath:null,index:'?',worktree:'?'},{path:'app-icon.png',oldPath:null,index:'M',worktree:'M'}]});
api.workspaceGitImage = async (_id, path, staged) => ({before:'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==', after:staged ? null : 'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=='});
api.workspaceGitDiff = async (_id, path, staged) => 'diff --git a/' + path + ' b/' + path + '\\n@@ -1,22 +1,22 @@\\n' + Array.from({length:20}, (_, i) => ' context ' + i).join('\\n') + '\\n-old\\n+' + (staged ? 'staged' : 'unstaged') + '\\n-long "' + 'x'.repeat(300) + '";\\n';
import './src/app.css';
let disk = '# Hello\\r\\n';
const files = new Map([['D:/demo/src/main.ts', 'const value = 1;\\n']]);
files.set('D:/demo/large.rs', '// ' + 'soft wrap '.repeat(80) + '\\n' + Array.from({length: 5000}, (_, i) => 'fn line_' + i + '() { let value = "hello"; }').join('\\n'));
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
  return { path, text, kind: /\.(ts|rs)$/.test(path) ? 'text' : 'markdown', size: text.length };
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
  await page.getByRole('textbox', {name:'文件内容编辑'}).waitFor();
  await page.getByRole('button', {name:'预览',exact:true}).click();
  await page.getByRole('heading', {name:'Hello'}).waitFor();
  console.log('preview loaded');
  const actions = page.locator('.workspace-actions');
  const actionToggle = page.getByLabel('更多文件操作');
  await actionToggle.click();
  assert.equal(await actions.evaluate(el => el.open), true);
  await page.locator('main').click({position:{x:300,y:200}});
  assert.equal(await actions.evaluate(el => el.open), false, 'outside click dismisses file actions');
  await actionToggle.click();
  await page.getByRole('button', {name:'查看源码',exact:true}).click();
  assert.equal(await actions.evaluate(el => el.open), false, 'choosing an action dismisses the menu');
  await actionToggle.click();
  await page.getByRole('button', {name:'查看预览',exact:true}).click();
  await actionToggle.click();
  await actionToggle.press('Escape');
  assert.equal(await actions.evaluate(el => el.open), false, 'Escape dismisses file actions');
  assert.equal(await page.locator('.workspace-picker').count(), 0, 'file picker collapses after selecting');
  assert.deepEqual(await page.evaluate(() => window.testDirectories()), [''], 'only load root initially');
  const bounds = await page.locator('.workspace-preview').boundingBox();
  const breadcrumb = await page.locator('.workspace-breadcrumb').boundingBox();
  const arrow = await page.locator('.workspace-breadcrumb > svg').last().boundingBox();
  assert.ok(arrow.y >= breadcrumb.y && arrow.y + arrow.height <= breadcrumb.y + breadcrumb.height, 'chevron stays in breadcrumb row');
  assert.equal(await page.locator('.workspace-preview .markdown').evaluate(el => getComputedStyle(el).fontSize), '15px');
  assert.ok(bounds.height > 650, 'content should occupy most of an 800px window');
  await page.getByRole('button', {name:/^产物 ·/}).click();
  assert.equal(await page.getByRole('region', {name:'会话产物'}).getByRole('button', {name:'README.md'}).count(), 1);
  await page.getByRole('button', {name:'选择项目文件',exact:true}).click();
  await page.getByRole('textbox', {name:'按文件名搜索项目'}).fill('hidden-result');
  await page.locator('.workspace-search-results').getByRole('button', {name:/hidden-result.md/}).waitFor();
  assert.deepEqual(await page.evaluate(() => window.testSearches()), ['hidden-result']);
  assert.deepEqual(await page.evaluate(() => window.testDirectories()), [''], 'search finds files without expanding directories');
  await page.getByRole('textbox', {name:'按文件名搜索项目'}).fill('');
  await page.getByRole('button', {name:'选择项目文件',exact:true}).click();
  await page.getByRole('button', {name:'编辑',exact:true}).click();
  await page.getByRole('textbox', {name:'文件内容编辑'}).fill('# Edited\n');
  assert.equal(await page.evaluate(() => window.testDisk()), '# Hello\r\n', 'typing must not save');
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
  await page.getByRole('textbox', {name:'文件内容编辑'}).fill('const changed = 2;\n');
  await page.getByRole('tab', {name:/README.md/}).click();
  await page.getByRole('heading', {name:'Draft'}).waitFor();
  await page.getByRole('tab', {name:/main.ts/}).click();
  assert.equal(await page.evaluate(() => window.testCode().state.doc.toString()), 'const changed = 2;\n');
  const readsBefore = await page.evaluate(() => window.testReads().length);
  await page.getByRole('button', {name:'选择项目文件',exact:true}).click();
  await page.getByRole('treeitem', {name:'main.ts',exact:true}).click();
  assert.equal(await page.getByRole('tab').count(), 2, 'opening same file selects existing tab');
  assert.equal(await page.evaluate(() => window.testReads().length), readsBefore, 'tab switch does not reread disk');
    await page.getByRole('button', {name:'关闭 main.ts',exact:true}).click();
    await page.locator('.workspace-close-modal .modal-foot').getByRole('button', {name:'取消',exact:true}).click();
    assert.equal(await page.getByRole('tab').count(), 2, 'cancel closing dirty tab retains it');
  await page.getByRole('button', {name:'选择项目文件',exact:true}).click();
  if (process.env.TEST_SCREENSHOT) await page.screenshot({ path: process.env.TEST_SCREENSHOT });
  await page.getByRole('treeitem', {name:'slow.md',exact:true}).click();
  await page.getByRole('tab', {name:/README.md/}).click();
  await page.waitForTimeout(350);
  assert.equal(await page.getByRole('tab').count(), 2, 'late file response cannot steal active tab');
  await page.getByRole('heading', {name:'Draft'}).waitFor();
    await page.getByRole('button', {name:'关闭 main.ts',exact:true}).click();
    await page.locator('.workspace-close-modal .modal-foot').getByRole('button', {name:'不保存',exact:true}).click();
    assert.equal(await page.getByRole('tab').count(), 1);
  await page.locator('.md-file-ref').click();
  await page.getByRole('tab', {name:'main.ts',exact:true}).waitFor();
  await page.getByRole('button', {name:'关闭 main.ts',exact:true}).click();
  await page.getByRole('button', {name:'关闭文件面板'}).click();
  await page.getByRole('button', {name:'打开面板'}).click();
  await page.getByRole('button', {name:/^产物 ·/}).click();
  await page.getByRole('region', {name:'会话产物'}).getByRole('button', {name:'README.md'}).click();
  assert.equal(await page.evaluate(() => window.testCode().state.doc.toString()), '# Draft\n');
  console.log('draft restored');
  await page.evaluate(() => window.changeDisk());
  await page.getByRole('button', {name:'保存',exact:true}).click();
  await page.getByRole('alert').filter({hasText:'外部修改'}).waitFor();
  console.log('conflict protected');
  assert.equal(await page.evaluate(() => window.testCode().state.doc.toString()), '# Draft\n');
  assert.equal(await page.evaluate(() => window.testDisk()), '# external\r\n');
  await page.getByRole('button', {name:'选择项目文件',exact:true}).click();
  await page.locator('.workspace-tree .workspace-type-icon.folder').waitFor();
  await page.locator('.workspace-tree .workspace-type-icon.image').waitFor();
  await page.evaluate(() => window.dispatchEvent(new CustomEvent('nova:preview-file', {detail: {path:'large.rs'}})));
  await page.getByRole('tab', {name:'large.rs',exact:true}).waitFor();
  await page.waitForFunction(() => document.querySelector('.cm-line span'));
  assert.ok(await page.locator('.cm-line').count() < 200, 'long files render only the viewport');
  assert.ok(await page.locator('.cm-lineNumbers .cm-gutterElement').count() > 1, 'line numbers are visible');
  assert.equal(await page.evaluate(() => window.testCode().state.doc.lines), 5001);
  const wrapped = await page.locator('.cm-line').first().evaluate(el => ({height:el.getBoundingClientRect().height,lineHeight:parseFloat(getComputedStyle(el).lineHeight)}));
  assert.ok(wrapped.height > wrapped.lineHeight * 2, 'long source line soft-wraps');
  // Select from the second visual row into the third, then verify the actual copied text.
  const points = await page.evaluate(() => {
    const view = window.testCode();
    const top = view.coordsAtPos(0).top;
    const positions = [];
    for (let i = 1; i < view.state.doc.line(1).to; i++) {
      const box = view.coordsAtPos(i);
      if (box && box.top > top + view.defaultLineHeight / 2 && (!positions.length || box.top > positions[0].top + view.defaultLineHeight / 2)) positions.push({pos:i,x:box.left,y:(box.top+box.bottom)/2,top:box.top});
      if (positions.length === 2) break;
    }
    return positions;
  });
  assert.equal(points.length, 2);
  await page.mouse.move(points[0].x, points[0].y);
  await page.mouse.down();
  await page.mouse.move(points[1].x, points[1].y, {steps:12});
  await page.mouse.up();
  const selected = await page.evaluate(() => {
    const view = window.testCode(); const range = view.state.selection.main;
    const data = new DataTransfer(); view.contentDOM.dispatchEvent(new ClipboardEvent('copy', {clipboardData:data,bubbles:true,cancelable:true}));
    return {from:range.from,to:range.to,copied:data.getData('text/plain'),text:view.state.sliceDoc(range.from,range.to),userSelect:getComputedStyle(view.contentDOM).userSelect};
  });
  assert.equal(selected.from, points[0].pos, JSON.stringify(selected));
  assert.equal(selected.to, points[1].pos, JSON.stringify(selected));
  assert.equal(selected.copied, selected.text);
  assert.equal(selected.userSelect, 'text', 'editor must not inherit the app shell selection lock');
  assert.equal(await page.evaluate(() => window.testCode().state.doc.lines), 5001, 'soft wrap does not insert newlines');
  assert.ok(await page.locator('.cm-line span').evaluateAll(spans => spans.some(el => getComputedStyle(el).color !== getComputedStyle(el.closest('.cm-content')).color)), 'Rust syntax is colored');
  const minimap = page.locator('[aria-label="代码缩略图"]');
  await minimap.waitFor();
  await page.waitForFunction(() => {
    const canvas = document.querySelector('.workspace-minimap canvas');
    if (!canvas || canvas.height < 100) return false;
    const {width, height} = canvas;
    const pixels = canvas.getContext('2d').getImageData(0, 0, width, height).data;
    let blankRows = 0;
    for (let y = 0; y < height; y++) {
      let painted = false;
      for (let x = 0; x < width; x++) if (pixels[(y * width + x) * 4 + 3]) { painted = true; break; }
      if (!painted) blankRows++;
    }
    return blankRows > height * .35 && blankRows < height * .9;
  });
  const mapBounds = await minimap.boundingBox();
  await minimap.click({position:{x:mapBounds.width / 2,y:mapBounds.height * .8}});
  await page.waitForFunction(() => window.testCode().scrollDOM.scrollTop > 0);
  for (const fraction of [.2, .85, .45]) {
    await minimap.click({position:{x:mapBounds.width / 2,y:mapBounds.height * fraction}});
    await page.waitForFunction(fraction => {
      const view = window.testCode();
      const target = view.state.doc.line(Math.floor(fraction * view.state.doc.lines) + 1);
      if (Math.abs(view.state.doc.lineAt(view.state.selection.main.head).number - target.number) > 1) return false;
      const position = view.coordsAtPos(target.from);
      const bounds = view.scrollDOM.getBoundingClientRect();
      return position && Math.abs((position.top + position.bottom - bounds.top - bounds.bottom) / 2) < view.defaultLineHeight * 4;
    }, fraction);
  }
  await page.evaluate(() => window.dispatchEvent(new CustomEvent('nova:preview-file', {detail: {path:'large.rs',line:4900}})));
  await page.waitForFunction(() => window.testCode().state.doc.lineAt(window.testCode().state.selection.main.head).number === 4900);
  await page.waitForFunction(() => [...document.querySelectorAll('.cm-line')].some(el => el.textContent.includes('fn line_4898')));
  assert.ok(await page.locator('.cm-line').count() < 200, 'line reveal remains virtualized');
  await page.waitForFunction(() => [...document.querySelectorAll('.cm-line span')].some(el => el.textContent === 'fn'));
  if (process.env.TEST_SCREENSHOT) await page.screenshot({ path: process.env.TEST_SCREENSHOT });
  await page.setViewportSize({width:700,height:800});
  assert.ok(await page.locator('.cm-scroller').evaluate(el => el.scrollWidth <= el.clientWidth + 1), 'narrow editor has no horizontal overflow');
  const panel = await page.locator('.workspace-panel').boundingBox();
  assert.ok(panel.x >= 0 && panel.x + panel.width <= 701);
  // A medium file previously occupied only the top half of the fixed-scale minimap.
  await page.evaluate(() => {
    const view = window.testCode();
    view.dispatch({changes: {from:0,to:view.state.doc.length,insert:Array.from({length:120}, (_, i) => `fn medium_${i}() { let value = "hello"; }`).join('\n')}});
  });
  await page.setViewportSize({width:700,height:1000});
  await page.waitForFunction(() => {
    const canvas = document.querySelector('.workspace-minimap canvas');
    if (!canvas || Math.abs(canvas.height - canvas.clientHeight * devicePixelRatio) > 1) return false;
    const pixels = canvas.getContext('2d').getImageData(0, Math.floor(canvas.height * .99), canvas.width, Math.max(1, Math.floor(canvas.height * .01))).data;
    return pixels.some((value, index) => index % 4 === 3 && value > 0);
  });
  const fullMapBounds = await minimap.boundingBox();
  await minimap.click({position:{x:fullMapBounds.width / 2,y:fullMapBounds.height * .98}});
  await page.waitForFunction(() => {
    const scroll = window.testCode().scrollDOM;
    return scroll.scrollTop >= (scroll.scrollHeight - scroll.clientHeight) * .95;
  });
  await minimap.press('Home');
  await page.waitForFunction(() => window.testCode().scrollDOM.scrollTop === 0);
  await minimap.press('End');
  await page.waitForFunction(() => {
    const map = document.querySelector('.workspace-minimap').getBoundingClientRect();
    const overlay = document.querySelector('.workspace-minimap-viewport').getBoundingClientRect();
    return Math.abs(map.bottom - overlay.bottom) < 3;
  });
  if (process.env.TEST_SCREENSHOT) await page.screenshot({path:process.env.TEST_SCREENSHOT});
  await page.evaluate(() => {
    const view = window.testCode();
    view.dispatch({changes:{from:0,to:view.state.doc.length,insert:Array.from({length:600}, (_, i) => `// line ${i} ` + 'wrapped '.repeat(i % 7 === 0 ? 100 : 3)).join('\n')}});
  });
  const jumpBounds = await minimap.boundingBox();
  for (const fraction of [.15, .8, .35]) {
    await minimap.click({position:{x:jumpBounds.width / 2,y:jumpBounds.height * fraction}});
    await page.waitForFunction(fraction => {
      const view = window.testCode();
      const line = view.state.doc.lineAt(view.state.selection.main.head);
      if (Math.abs(line.number - (Math.floor(fraction * view.state.doc.lines) + 1)) > 1) return false;
      const position = view.coordsAtPos(line.from);
      const bounds = view.scrollDOM.getBoundingClientRect();
      return position && Math.abs((position.top + position.bottom - bounds.top - bounds.bottom) / 2) < view.defaultLineHeight * 3;
    }, fraction);
  }
  for (const theme of ['ink-dark', 'ink-light']) {
    await page.evaluate(theme => {
      document.documentElement.dataset.theme = theme;
      const view = window.testCode();
      const from = view.state.selection.main.head;
      view.dispatch({selection:{anchor:from,head:view.state.doc.line(view.state.doc.lineAt(from).number + 3).to}});
      view.focus();
    }, theme);
    await page.locator('.cm-selectionBackground').first().waitFor();
    const colors = await page.locator('.cm-selectionBackground').first().evaluate(el => {
      const canvas = document.createElement('canvas'); canvas.width = canvas.height = 1;
      const ctx = canvas.getContext('2d');
      const rgba = color => {ctx.clearRect(0,0,1,1);ctx.fillStyle=color;ctx.fillRect(0,0,1,1);return [...ctx.getImageData(0,0,1,1).data];};
      return {selection:rgba(getComputedStyle(el).backgroundColor),panel:rgba(getComputedStyle(document.querySelector('.workspace-panel')).backgroundColor),editor:rgba(getComputedStyle(document.querySelector('.cm-editor')).backgroundColor)};
    });
    assert.ok(colors.selection[3] > 40 && colors.selection[3] < 90, `${theme}: selection uses translucent theme accent, not opaque default`);
    assert.ok(colors.panel[3] > 195 && colors.panel[3] < 225, 'panel retains a subtle backdrop');
    assert.equal(colors.editor[3], 0, 'editor must not obscure the translucent panel');
    if (process.env.TEST_SCREENSHOT) await page.screenshot({path:process.env.TEST_SCREENSHOT.replace('.png', `-${theme}.png`)});
  }
  assert.deepEqual(errors, []);
  await page.evaluate(() => {
    const view = window.testCode();
    view.dispatch({changes:{from:0,to:view.state.doc.length,insert:'fn short() {}'}});
  });
  await page.waitForFunction(() => getComputedStyle(document.querySelector('.workspace-minimap')).display === 'none');
  await page.setViewportSize({width:1280,height:820});
  const divider = page.getByRole('separator', {name:'调整文件面板宽度'});
  await divider.press('ArrowLeft');
  const rememberedRatio = await page.evaluate(() => window.testLayout().widthRatio);
  await page.getByRole('button', {name:'关闭文件面板'}).click();
  await page.getByRole('button', {name:'打开面板'}).click();
  assert.equal(await page.evaluate(() => window.testLayout().widthRatio), rememberedRatio);
  await page.setViewportSize({width:1600,height:900});
  await page.waitForFunction(ratio => Math.abs(document.querySelector('.workspace-panel').clientWidth - document.querySelector('.workspace-panel').parentElement.clientWidth * ratio) < 3, rememberedRatio);
  await page.evaluate(() => window.showLayoutSettings());
  await page.getByRole('checkbox', {name:'代码软换行',exact:true}).uncheck();
  await page.getByRole('checkbox', {name:'超过一屏时显示代码缩略图'}).uncheck();
  await page.getByRole('slider', {name:'侧栏宽度比例'}).fill('60');
  await page.getByRole('spinbutton', {name:'窗口宽度',exact:true}).fill('1400');
  await page.getByRole('button', {name:'应用窗口尺寸'}).click();
  await page.getByRole('checkbox', {name:'最大化窗口',exact:true}).check();
  await page.getByRole('checkbox', {name:'记住窗口大小、位置及最大化状态'}).uncheck();
  assert.equal(await page.evaluate(() => window.testLayout().widthRatio), .6);
  await page.getByRole('button', {name:'重置布局'}).click();
  await page.waitForFunction(() => window.testLayout().widthRatio === null && window.testLayout().softWrap && window.testLayout().minimap && !window.testLayout().open);
  assert.equal(await page.getByRole('spinbutton', {name:'窗口宽度',exact:true}).inputValue(), '1280');
  assert.equal(await page.getByRole('checkbox', {name:'记住窗口大小、位置及最大化状态'}).isChecked(), true);
  if (process.env.TEST_SCREENSHOT) await page.screenshot({path:process.env.TEST_SCREENSHOT.replace('.png','-settings.png')});
  await page.evaluate(() => window.hideLayoutSettings());
  await page.getByRole('button', {name:/^产物 ·/}).click();
  await page.evaluate(() => window.loadHistory());
  await page.getByRole('region', {name:'会话产物'}).getByRole('button', {name:'generated.ts'}).waitFor();
  await page.getByRole('button', {name:'Git 变动',exact:true}).click();
  await page.locator('.workspace-git-files').getByRole('button', {name:'M src/main.ts',exact:true}).first().click();
  await page.getByLabel('文件差异').getByText('unstaged', {exact:true}).waitFor();
  const fold = page.getByRole('button', {name:'展开 14 行未变动内容'});
  await fold.click();
  await page.getByLabel('文件差异').getByText('context 10', {exact:true}).waitFor();
  await page.getByRole('button', {name:'折叠 14 行未变动内容'}).click();
  assert.equal(await page.getByLabel('文件差异').getByText('context 10', {exact:true}).count(), 0);
  await page.locator('.workspace-git-files').getByRole('button', {name:'M src/main.ts',exact:true}).last().click();
  await page.getByLabel('文件差异').getByText('staged', {exact:true}).waitFor();
  await page.getByRole('button', {name:'展开全部',exact:true}).click();
  await page.getByLabel('文件差异').getByText('context 10', {exact:true}).waitFor();
  assert.ok(await page.locator('.workspace-diff').evaluate(el => el.scrollWidth <= el.clientWidth + 1), 'Git 差异区自动换行，不出现横向滚动条');
  const longRow = page.locator('.workspace-diff-line').filter({hasText:'long "'}).last();
  assert.ok(await longRow.evaluate(el => el.getBoundingClientRect().height > parseFloat(getComputedStyle(el).lineHeight) * 2), '超长差异行折行显示');
  if (process.env.TEST_SCREENSHOT) await page.screenshot({path:process.env.TEST_SCREENSHOT.replace('.png','-git.png')});
  const imageDiff = page.getByLabel('图片差异');
  await page.locator('.workspace-git-files').getByRole('button', {name:'M app-icon.png',exact:true}).first().click();
  await imageDiff.locator('img').first().waitFor();
  const imageSources = await imageDiff.locator('img').evaluateAll(imgs => imgs.map(img => img.getAttribute('src')));
  assert.equal(imageSources.length, 2, '未暂存图片并排显示新旧两版');
  assert.ok(imageSources.every(src => src.startsWith('data:image/png;base64,')), '图片直接内联显示，不回落到二进制差异文本');
  assert.equal(await imageDiff.getByText('修改前', {exact:true}).count(), 1);
  assert.equal(await page.locator('.workspace-diff-line').count(), 0, '图片不再渲染 Binary files differ 一类的文本差异');
  assert.equal(await page.locator('.workspace-diff-count').count(), 0, '图片对比不显示文本行数统计');
  await page.waitForFunction(() => {
    const imgs = [...document.querySelectorAll('.workspace-image-diff img')];
    return imgs.length === 2 && imgs.every(img => img.complete && img.naturalWidth === 1);
  });
  await page.locator('.workspace-git-files').getByRole('button', {name:'M app-icon.png',exact:true}).last().click();
  await imageDiff.getByText('已删除', {exact:true}).waitFor();
  assert.equal(await imageDiff.locator('img').count(), 1, '暂存后工作区图缺失时只显示旧版本');
  if (process.env.TEST_SCREENSHOT) await page.screenshot({path:process.env.TEST_SCREENSHOT.replace('.png','-git-image.png')});
  const draftBeforeGitClose = await page.evaluate(() => window.testCode().state.doc.toString());
  await page.getByRole('tab', {name:/large.rs/}).click();
  await page.getByRole('textbox', {name:'文件内容编辑'}).waitFor();
  assert.equal(await page.evaluate(() => window.testCode().state.doc.toString()), draftBeforeGitClose, 'Git browsing preserves the open file draft');
  assert.deepEqual(errors, []);
  console.log('Workspace panel: virtualized Rust editor, highlighting, line numbers, soft wrap, minimap, line reveal, multi-tab drafts, explicit save and conflict passed');
} finally {
  await browser?.close();
  server?.kill();
  await Promise.all([`${name}.html`, `${name}.tsx`].map(path => rm(path, {force:true})));
}
