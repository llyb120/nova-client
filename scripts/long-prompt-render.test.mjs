import assert from 'node:assert/strict';
import { writeFile, rm, access } from 'node:fs/promises';
import { spawn } from 'node:child_process';
import { chromium } from 'playwright-core';

// Run with node scripts/long-prompt-render.test.mjs (real Edge/Chrome canvas).
const name = `long-prompt-check-${process.pid}`;
let server, browser;
try {
  await writeFile(`${name}.html`, `<div id="root"></div><script type="module" src="/${name}.tsx"></script>`);
  await writeFile(`${name}.tsx`, `
import { render } from 'solid-js/web';
import { createSignal } from 'solid-js';
import { CanvasTranscript } from './src/components/CanvasTranscript';
import { setState, state } from './src/store';
import './src/app.css';
const [session, setSession] = createSignal({id:'empty',groups:[]});
let handle, completed = false, lastFrame = performance.now(), maxGap = 0, frames = 0, longestMeasure = 0, stream, started = 0, previewMs = 0, foldPoint, collapsePoint;
const fill = CanvasRenderingContext2D.prototype.fillText;
CanvasRenderingContext2D.prototype.fillText = function(text,x,y,...args) {
  if (text.startsWith('展开完整提示词')) { if (!previewMs) previewMs=performance.now()-started; foldPoint={x,y}; }
  if (text === '收起长提示词') collapsePoint={x,y};
  return fill.call(this,text,x,y,...args);
};
const measure = CanvasRenderingContext2D.prototype.measureText;
CanvasRenderingContext2D.prototype.measureText = function(text) {
  longestMeasure = Math.max(longestMeasure, text.length);
  return measure.call(this, text);
};
function tick(now) { maxGap = Math.max(maxGap, now-lastFrame); lastFrame = now; frames++; requestAnimationFrame(tick); }
requestAnimationFrame(tick);
window.loadPrompt = (kind) => {
  clearInterval(stream);
  setState('expanded', 'user-text-1', false);
  const text = kind === 'continuous' ? Array.from({length:724000}, (_,i) => String.fromCharCode(0x4e00 + i % 1800)).join('')
    : ('启动提示词 😀 abc ' + 'long content '.repeat(12) + '\\n').repeat(5000).slice(0,724000);
  completed = false; maxGap = 0; frames = 0; longestMeasure = 0; lastFrame = performance.now();
  started=lastFrame; previewMs=0; foldPoint=null;
  setSession({id:kind,groups:[{user:{type:'user',id:1,text,ts:0},body:[],
    ...(kind === 'paragraphs' ? {turn:{type:'turn',id:4,ts:0,durationMs:1000,stopReason:'end'}} : {})}]});
};
window.startStream = () => {
  let n = 0;
  stream = setInterval(() => setSession(previous => ({...previous,groups:[{
    ...previous.groups[0],body:[{type:'assistant',id:3,ts:0,text:'response '+ ++n}]
  }]})), 80);
};
window.stopStream = () => clearInterval(stream);
window.metrics = () => ({completed, maxGap, frames, longestMeasure, previewMs, foldPoint, collapsePoint, open:!!state.expanded['user-text-1']});
window.bottom = () => { collapsePoint=null; handle.scrollToBottom(); };
window.switchAway = () => { clearInterval(stream); setSession({id:'small',groups:[{user:{type:'user',id:2,text:'short',ts:0},body:[]}]}); };
window.scrollPrompt = () => handle.scrollToGroup(0);
render(() => <><button onClick={() => {window.switchAway(); document.body.dataset.switched='yes';}}>Switch away</button><div style="height:700px;display:flex"><CanvasTranscript ref={value => handle=value}
  threadId={session().id} groups={session().groups} permissions={[]} running={false} loading={false}
  preview={false} onReturnToCurrent={() => {}} emptyHint="ready"
  onScroll={(_top,max) => { if (max > 10000) completed = true; }}/></div></>, document.getElementById('root'));
`);
  const port = 15000 + process.pid % 1000;
  server = spawn(process.execPath, ['node_modules/vite/bin/vite.js', '--host', '127.0.0.1', '--port', String(port)], {
    windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'], env: {...process.env, NO_COLOR:'1'},
  });
  await new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(Error('Vite startup timed out')), 20000);
    server.stdout.on('data', data => { if (data.toString().includes('Local:')) { clearTimeout(timeout); resolve(); } });
    server.on('exit', code => { clearTimeout(timeout); reject(Error('Vite exited: '+code)); });
  });
  let executablePath;
  for (const path of [process.env.TEST_BROWSER, 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe', 'C:/Program Files/Google/Chrome/Application/chrome.exe'].filter(Boolean)) {
    try { await access(path); executablePath = path; break; } catch {}
  }
  assert.ok(executablePath, 'Set TEST_BROWSER to a Chromium executable');
  browser = await chromium.launch({executablePath, headless:true});
  const page = await browser.newPage({viewport:{width:1280,height:800}});
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.goto(`http://127.0.0.1:${port}/${name}.html`, {waitUntil:'networkidle'});
  for (const kind of ['continuous', 'paragraphs']) {
    await page.evaluate(kind => window.loadPrompt(kind), kind);
    await page.waitForFunction(() => window.metrics().previewMs > 0, null, {timeout:1000});
    const preview = await page.evaluate(() => window.metrics());
    console.log(kind, 'first preview', preview.previewMs, 'ms');
    assert.ok(preview.previewMs < 500, 'First content must not wait for full layout');
    assert.equal(preview.completed, false, 'Hidden full text must not be laid out');
    const canvas = await page.locator('canvas.transcript-canvas-only').boundingBox();
    await page.mouse.click(canvas.x + preview.foldPoint.x + 10, canvas.y + preview.foldPoint.y);
    assert.equal(await page.evaluate(() => window.metrics().open), true);
    // A second click during layout must cancel, not get swallowed as text selection.
    await page.mouse.click(canvas.x + preview.foldPoint.x + 10, canvas.y + preview.foldPoint.y);
    assert.equal(await page.evaluate(() => window.metrics().open), false);
    await page.waitForTimeout(150);
    await page.mouse.click(canvas.x + preview.foldPoint.x + 10, canvas.y + preview.foldPoint.y);
    // Expansion must work on an idle/completed session without an unrelated stream update.
    await page.waitForFunction(() => window.metrics().completed, null, {timeout:60000});
    await page.evaluate(() => window.stopStream());
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(resolve)));
    const metrics = await page.evaluate(() => window.metrics());
    console.log(kind, metrics);
    assert.ok(metrics.maxGap < 250, `UI blocked for ${metrics.maxGap}ms`);
    assert.ok(metrics.longestMeasure < 4096, 'Never shape the entire 724k token');
    await page.evaluate(() => window.bottom());
    await page.waitForFunction(() => window.metrics().collapsePoint !== null);
    const collapse = await page.evaluate(() => window.metrics().collapsePoint);
    await page.mouse.click(canvas.x+collapse.x+10, canvas.y+collapse.y);
    await page.waitForFunction(() => !window.metrics().open);
    await page.waitForTimeout(150);
    await page.evaluate(() => window.scrollPrompt());
  }
  await page.evaluate(() => window.loadPrompt('continuous'));
  await page.waitForFunction(() => window.metrics().previewMs > 0);
  const fold = await page.evaluate(() => window.metrics().foldPoint);
  const bounds = await page.locator('canvas.transcript-canvas-only').boundingBox();
  await page.mouse.click(bounds.x+fold.x+10, bounds.y+fold.y);
  await page.getByRole('button', {name:'Switch away'}).click({timeout:1000});
  assert.equal(await page.getAttribute('body', 'data-switched'), 'yes');
  await page.waitForTimeout(200);
  assert.equal(await page.evaluate(() => window.metrics().completed), false, 'Cancelled layout must not replace the new session');
  await page.evaluate(() => window.loadPrompt('continuous'));
  await page.setViewportSize({width:1100,height:800});
  await page.waitForFunction(() => window.metrics().previewMs > 0, null, {timeout:1000});
  console.log('Switching away cancels cold layout; returning and resizing render successfully');
  assert.deepEqual(errors, []);
} finally {
  await browser?.close();
  server?.kill();
  await Promise.all([`${name}.html`, `${name}.tsx`].map(path => rm(path, {force:true})));
}
