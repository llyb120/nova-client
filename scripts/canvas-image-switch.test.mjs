import assert from "node:assert/strict";
import { access, rm, writeFile } from "node:fs/promises";
import { spawn } from "node:child_process";
import { chromium } from "playwright-core";

const name = `canvas-image-switch-check-${process.pid}`;
let server, browser;
try {
  await writeFile(`${name}.html`, `<div id="root"></div><script type="module" src="/${name}.tsx"></script>`);
  await writeFile(`${name}.tsx`, `
import { render } from 'solid-js/web';
import { createEffect, createSignal } from 'solid-js';
import { CanvasTranscript } from './src/components/CanvasTranscript';
import './src/app.css';
const group = (text, images = []) => ({user:{type:'user',id:1,text,images,ts:0},body:[],turn:{type:'turn',id:2,ts:0,stopReason:'end'}});
const [session, setSession] = createSignal({id:'previous',groups:[group('previous message')],loading:false});
let handle, frames = [], pending = [], holdImages = true;
const decode = HTMLImageElement.prototype.decode;
HTMLImageElement.prototype.decode = async function() {
  await decode.call(this);
  if (holdImages) await new Promise((resolve,reject) => pending.push({resolve,reject}));
};
const clear = CanvasRenderingContext2D.prototype.clearRect;
const fill = CanvasRenderingContext2D.prototype.fillText;
const draw = CanvasRenderingContext2D.prototype.drawImage;
CanvasRenderingContext2D.prototype.clearRect = function(...args) {
  if (this.canvas.classList.contains('transcript-canvas-only')) frames.push({texts:[],images:0});
  return clear.apply(this,args);
};
CanvasRenderingContext2D.prototype.fillText = function(...args) {
  if (this.canvas.classList.contains('transcript-canvas-only')) frames.at(-1)?.texts.push(args[0]);
  return fill.apply(this,args);
};
CanvasRenderingContext2D.prototype.drawImage = function(...args) {
  if (this.canvas.classList.contains('transcript-canvas-only') && frames.length) frames.at(-1).images++;
  return draw.apply(this,args);
};
window.switchThread = (id, image = false, loading = false) => {
  frames = [];
  holdImages = true;
  const count = typeof image === 'number' ? image : image ? 1 : 0;
  const images = Array.from({length:count},(_,i)=>({name:id+i,mimeType:'image/svg+xml',
    data:btoa(image === 'broken' ? 'not an image' : '<svg xmlns="http://www.w3.org/2000/svg" width="'+(i ? 400 : 1200)+'" height="400"><rect width="1200" height="400" fill="red"/></svg>')}));
  const g = group(id, image === 'generated' ? [] : images);
  if (image === 'generated') {
    const sample = document.createElement('canvas');
    sample.width = 1200; sample.height = 400;
    sample.getContext('2d').fillRect(0,0,1200,400);
    g.body.push({type:'assistant',id:3,text:'![generated]('+sample.toDataURL()+')',ts:0});
  }
  setSession({id,groups:loading ? [] : [g],loading});
};
window.releaseImages = (count = pending.length) => { holdImages = count < pending.length; pending.splice(0,count).forEach(({resolve}) => resolve()); };
window.rejectImages = () => { holdImages = false; pending.splice(0).forEach(({reject}) => reject(Error('decode failed'))); };
window.stats = () => ({frames,pending:pending.length,width:document.querySelector('canvas.transcript-canvas-only').width,height:document.querySelector('canvas.transcript-canvas-only').height});
window.repaintWhilePending = async () => {
  const before = window.stats();
  handle.scrollToBottom();
  const canvas = document.querySelector('canvas.transcript-canvas-only');
  const rect = canvas.getBoundingClientRect();
  canvas.dispatchEvent(new MouseEvent('mousemove',{clientX:rect.left+100,clientY:rect.top+40}));
  document.getElementById('fixture').style.height = '650px';
  await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
  return {before,after:window.stats()};
};
render(() => {
  // ChatView schedules the same pin in a microtask whenever the session items change.
  createEffect(() => { session(); queueMicrotask(() => handle?.scrollToBottom()); });
  return <div id="fixture" style="height:600px;width:900px;display:flex"><CanvasTranscript ref={value=>handle=value}
    threadId={session().id} groups={session().groups} loading={session().loading} permissions={[]}
    running={false} preview={false} onReturnToCurrent={()=>{}} emptyHint="empty"/></div>;
},document.getElementById('root'));
`);
  const port = 16000 + process.pid % 1000;
  server = spawn(process.execPath, ["node_modules/vite/bin/vite.js", "--host", "127.0.0.1", "--port", String(port), "--strictPort"], {
    windowsHide: true, stdio: ["ignore", "pipe", "pipe"], env: { ...process.env, NO_COLOR: "1" },
  });
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(Error("Vite startup timed out")), 20000);
    server.stdout.on("data", data => { if (data.toString().includes("Local:")) { clearTimeout(timer); resolve(); } });
    server.on("error", error => { clearTimeout(timer); reject(error); });
    server.on("exit", code => { clearTimeout(timer); reject(Error(`Vite exited: ${code}`)); });
  });
  let executablePath;
  for (const path of [process.env.TEST_BROWSER, "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe", "C:/Program Files/Google/Chrome/Application/chrome.exe"].filter(Boolean)) {
    try { await access(path); executablePath = path; break; } catch {}
  }
  assert.ok(executablePath, "Set TEST_BROWSER to a Chromium executable");
  browser = await chromium.launch({ executablePath, headless: true });
  const page = await browser.newPage({ viewport: { width: 1100, height: 800 } });
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  await page.goto(`http://127.0.0.1:${port}/${name}.html`, { waitUntil: "domcontentloaded", timeout: 60000 });
  const waitForText = text => page.waitForFunction(text => window.stats?.().frames.at(-1)?.texts.includes(text), text, { timeout: 60000 });
  await waitForText("previous message");

  await page.evaluate(() => window.switchThread("image message", true));
  await page.waitForFunction(() => window.stats().pending === 1);
  await page.waitForTimeout(650);
  assert.deepEqual(await page.evaluate(() => window.stats().frames), [], "Slow image decoding must not commit a placeholder frame, including after the old 300ms deadline");
  const { before, after } = await page.evaluate(() => window.repaintWhilePending());
  assert.deepEqual(after.frames, [], "Hover, scroll and ResizeObserver must preserve the retained frame");
  assert.equal(after.width, before.width);
  assert.equal(after.height, before.height, "Resizing the backing store would erase retained pixels before the layout is ready");
  await page.evaluate(() => window.releaseImages());
  await waitForText("image message");
  const ready = await page.evaluate(() => window.stats());
  assert.ok(ready.frames.every(frame => frame.images === 1 && frame.texts.includes("image message")), "The first committed frame must contain the decoded image and its final text layout");
  assert.notEqual(ready.height, before.height, "Commit must also apply the deferred backing-store resize");

  await page.evaluate(() => window.switchThread("multiple images", 2));
  await page.waitForFunction(() => window.stats().pending === 2);
  await page.evaluate(() => window.releaseImages(1));
  await page.waitForTimeout(650);
  assert.deepEqual(await page.evaluate(() => window.stats().frames), [], "One decoded image must not commit a partial multi-image layout");
  await page.evaluate(() => window.releaseImages());
  await waitForText("multiple images");
  assert.ok((await page.evaluate(() => window.stats().frames)).every(frame => frame.images === 2));

  await page.evaluate(() => window.switchThread("generated image", "generated"));
  await page.waitForFunction(() => window.stats().pending === 1);
  await page.waitForTimeout(650);
  assert.deepEqual(await page.evaluate(() => window.stats().frames), [], "Generated images must also wait for decoding, not paint loading text first");
  await page.evaluate(() => window.releaseImages());
  await waitForText("generated image");
  assert.ok((await page.evaluate(() => window.stats().frames)).every(frame => frame.images === 1));

  await page.evaluate(() => window.switchThread("broken image", "broken"));
  await page.waitForFunction(() => window.stats().frames.at(-1)?.texts.includes("broken image"), null, { timeout: 2000 });
  assert.ok((await page.evaluate(() => window.stats().frames)).every(frame => frame.images === 0), "Load errors must settle without waiting for the watchdog");
  await page.evaluate(() => window.switchThread("decode error", true));
  await page.waitForFunction(() => window.stats().pending === 1);
  await page.evaluate(() => window.rejectImages());
  await page.waitForFunction(() => window.stats().frames.at(-1)?.texts.includes("decode error"), null, { timeout: 2000 });
  assert.ok((await page.evaluate(() => window.stats().frames)).every(frame => frame.images === 0), "Rejected decoding must not be treated as a drawable image");

  // A late decode from an abandoned session cannot release or overwrite the next frame.
  await page.evaluate(() => window.switchThread("abandoned image", true));
  await page.waitForFunction(() => window.stats().pending === 1);
  await page.evaluate(() => window.switchThread("text-only replacement"));
  await waitForText("text-only replacement");
  await page.evaluate(() => window.releaseImages());
  await page.waitForTimeout(350);
  assert.ok((await page.evaluate(() => window.stats().frames)).every(frame => frame.images === 0 && frame.texts.includes("text-only replacement")));

  // A cold switch still paints its loading state instead of freezing the old session.
  await page.evaluate(() => window.switchThread("cold", false, true));
  await waitForText("正在加载会话…");
  await page.evaluate(() => window.switchThread("cold", true));
  await page.waitForFunction(() => window.stats().pending === 1);
  await page.waitForTimeout(650);
  assert.ok((await page.evaluate(() => window.stats().frames)).every(frame => !frame.texts.includes("cold")), "Cold snapshots must not show a partial image layout either");
  await page.evaluate(() => window.releaseImages());
  await waitForText("cold");
  assert.equal(await page.evaluate(() => window.stats().frames.at(-1).images), 1);

  // A hung decoder is bounded, but its late result cannot cause another layout jump.
  await page.clock.install();
  await page.evaluate(() => window.switchThread("timed-out image", true));
  await page.waitForFunction(() => window.stats().pending === 1);
  await page.clock.fastForward(11_000);
  await waitForText("timed-out image");
  await page.evaluate(() => window.releaseImages());
  await page.clock.fastForward(200);
  await page.evaluate(() => window.repaintWhilePending());
  assert.ok((await page.evaluate(() => window.stats().frames)).every(frame => frame.images === 0 && frame.texts.includes("timed-out image")), "A timed-out source must not replace its settled placeholder on a late decode");
  assert.deepEqual(errors, []);
  console.log("Canvas image switch regression passed");
} finally {
  await browser?.close();
  server?.kill();
  await Promise.all([`${name}.html`, `${name}.tsx`].map(path => rm(path, { force: true })));
}
