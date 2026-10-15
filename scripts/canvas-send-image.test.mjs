import assert from "node:assert/strict";
import { access, rm, writeFile } from "node:fs/promises";
import { spawn } from "node:child_process";
import { chromium } from "playwright-core";

// 发送带图消息：输入框收起（宿主变高）与新消息必须同帧落屏，不能先画一帧下移的旧内容。
const name = `canvas-send-image-check-${process.pid}`;
let server, browser;
try {
  await writeFile(`${name}.html`, `<div id="root"></div><script type="module" src="/${name}.tsx"></script>`);
  await writeFile(`${name}.tsx`, `
import { render } from 'solid-js/web';
import { createEffect, createSignal } from 'solid-js';
import { CanvasTranscript } from './src/components/CanvasTranscript';
import { prewarmAttachmentImages } from './src/components/ImageAttachmentStrip';
import './src/app.css';
const svg = '<svg xmlns="http://www.w3.org/2000/svg" width="800" height="400"><rect width="800" height="400" fill="blue"/></svg>';
const image = {name:'pic',mimeType:'image/svg+xml',data:btoa(svg),size:svg.length};
const hist = Array.from({length:8},(_,i)=>({user:{type:'user',id:i*3+1,text:'q'+i,images:[],ts:0},
  body:[{type:'assistant',id:i*3+2,text:'answer '.repeat(80),ts:0}],turn:{type:'turn',id:i*3+3,ts:0,stopReason:'end'}}));
const [groups, setGroups] = createSignal(hist);
let handle; window.frames = [];
const clear = CanvasRenderingContext2D.prototype.clearRect, fill = CanvasRenderingContext2D.prototype.fillText;
const mine = c => c.canvas.classList.contains('transcript-canvas-only');
CanvasRenderingContext2D.prototype.clearRect = function(...a) { if (mine(this)) window.frames.push({h:this.canvas.height,texts:[]}); return clear.apply(this,a); };
CanvasRenderingContext2D.prototype.fillText = function(...a) { if (mine(this)) window.frames.at(-1)?.texts.push(a[0]); return fill.apply(this,a); };
window.attach = () => prewarmAttachmentImages([image]);
window.send = () => {
  window.frames = [];
  setGroups([...hist, {user:{type:'user',id:-1,text:'new message',images:[{...image}],ts:1},body:[]}]);
  document.getElementById('fx').style.height = '680px';
};
render(() => {
  createEffect(() => { groups(); queueMicrotask(() => handle?.scrollToBottom()); });
  return <div id="fx" style="height:600px;width:900px;display:flex"><CanvasTranscript ref={v=>handle=v}
    threadId="t" groups={groups()} loading={false} permissions={[]}
    running={true} preview={false} onReturnToCurrent={()=>{}} emptyHint="empty"/></div>;
},document.getElementById('root'));
`);
  const port = 18000 + process.pid % 1000;
  server = spawn(process.execPath, ["node_modules/vite/bin/vite.js", "--host", "127.0.0.1", "--port", String(port), "--strictPort"], {
    windowsHide: true, stdio: ["ignore", "pipe", "pipe"], env: { ...process.env, NO_COLOR: "1" },
  });
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(Error("Vite startup timed out")), 20000);
    server.stdout.on("data", data => { if (data.toString().includes("Local:")) { clearTimeout(timer); resolve(); } });
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
  await page.waitForFunction(() => window.frames.at(-1)?.texts.includes("q7"), null, { timeout: 60000 });
  await page.evaluate(() => window.attach());
  await page.waitForTimeout(200);
  await page.evaluate(() => window.send());
  await page.waitForFunction(() => window.frames.at(-1)?.texts.includes("new message"), null, { timeout: 2000 });
  const frames = await page.evaluate(() => window.frames);
  const tall = frames.filter(frame => frame.h === 680);
  assert.ok(tall.length && tall.every(frame => frame.texts.includes("new message")),
    "After the composer collapses, no frame may show the shifted old transcript without the sent message");
  assert.deepEqual(errors, []);
  console.log("Canvas send image regression passed");
} finally {
  await browser?.close();
  server?.kill();
  await Promise.all([`${name}.html`, `${name}.tsx`].map(path => rm(path, { force: true })));
}
