import assert from 'node:assert/strict';
import { writeFile, rm, access } from 'node:fs/promises';
import { spawn } from 'node:child_process';
import { chromium } from 'playwright-core';
const name = `workspace-stream-${process.pid}`;
let server, browser;
try {
  await writeFile(`${name}.html`, `<div id="root"></div><script type="module" src="/${name}.tsx"></script>`);
  await writeFile(`${name}.tsx`, `
import { render } from 'solid-js/web';
import { createSignal, Show } from 'solid-js';
import { CanvasTranscript } from './src/components/CanvasTranscript';
import WorkspacePanel from './src/components/WorkspacePanel';
import { setState } from './src/store';
import './src/app.css';
const [open,setOpen]=createSignal(false);
const [groups,setGroups]=createSignal([]);
let timer, calls=0, measureMs=0, paints=0, maxGap=0, frames=0, previous=0, commits=0;
const history=Array.from({length:160},(_,i)=>({user:{type:'user',id:10+i*3,ts:0,text:'历史问题 '+i},body:[{type:'assistant',id:11+i*3,ts:0,text:('历史回答 '+i+' 包含说明和源代码路径 src/main.ts。').repeat(80)}],turn:{type:'turn',id:12+i*3,ts:0,durationMs:1000,stopReason:'end'}}));
const measure=CanvasRenderingContext2D.prototype.measureText;
CanvasRenderingContext2D.prototype.measureText=function(text) {const start=performance.now(); const r=measure.call(this,text); calls++; measureMs+=performance.now()-start; return r;};
const clear=CanvasRenderingContext2D.prototype.clearRect;
CanvasRenderingContext2D.prototype.clearRect=function(...args) {paints++; return clear.apply(this,args);};
function tick(now) {if(previous) maxGap=Math.max(maxGap,now-previous); previous=now;frames++;requestAnimationFrame(tick);}
requestAnimationFrame(tick);
setState({currentId:'perf',cwd:'D:/demo',items:[],running:{perf:true}});
window.run=(show) => {
 clearInterval(timer);setOpen(show);calls=0;measureMs=0;paints=0;maxGap=0;frames=0;previous=0;commits=0;
 let n=0;
 const text=('这里是流式输出的过程内容，包含代码路径 src/main.ts 和日志记录。').repeat(500);
 timer=setInterval(()=>{n++; const item={type:'thought',id:2,ts:0,text:text+'继续输出 '+n};setState('items',[item]);setGroups([...history,{body:[item]}]);},30);
};
window.metrics=()=>({calls,measureMs,paints,maxGap,frames,commits,width:document.querySelector('.canvas-transcript-host').clientWidth});
window.stop=()=>clearInterval(timer);
setGroups(history);
render(()=><div class="chat-shell" style="height:100vh"><div class="chat-primary"><div class="chat-body"><CanvasTranscript threadId="perf" groups={groups()} permissions={[]} running loading={false} preview={false} onReturnToCurrent={()=>{}} onScroll={()=>commits++} emptyHint="ready" /></div></div><Show when={open()}><WorkspacePanel threadId="perf" request={null} onClose={()=>setOpen(false)}/></Show></div>,document.getElementById('root'));
`);
  const port=16000+process.pid%1000;
  server=spawn(process.execPath,['node_modules/vite/bin/vite.js','--host','127.0.0.1','--port',String(port)],{windowsHide:true,stdio:['ignore','pipe','pipe'],env:{...process.env,NO_COLOR:'1'}});
  await new Promise((resolve,reject)=>{
    const timeout=setTimeout(()=>reject(Error('Vite startup timed out')),20000);
    server.stdout.on('data',data=>{if(data.toString().includes('Local:')){clearTimeout(timeout);resolve();}});
    server.on('exit',code=>{clearTimeout(timeout);reject(Error('Vite exited: '+code));});
  });
  let executablePath;
  for(const path of [process.env.TEST_BROWSER,'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe','C:/Program Files/Google/Chrome/Application/chrome.exe'].filter(Boolean)) {
    try {await access(path);executablePath=path;break;}catch{}
  }
  assert.ok(executablePath);
  browser=await chromium.launch({executablePath,headless:true});
  const page=await browser.newPage({viewport:{width:1280,height:800}});
  const errors=[];
  page.on('pageerror',error=>errors.push(error.message));
  await page.goto(`http://127.0.0.1:${port}/${name}.html`,{waitUntil:'networkidle'});
  await page.waitForFunction(()=>window.metrics().commits>0);
  for(const show of [false,true,false,true]) {
    await page.evaluate(show=>window.run(show),show);
    await page.waitForTimeout(3000);
    const metrics=await page.evaluate(()=>{window.stop();return window.metrics();});
    console.log(show,metrics);
    assert.ok(metrics.commits >= 5, 'Streaming must commit layouts after opening/closing an empty sidebar');
    assert.ok(metrics.maxGap < 250, 'History reflow must keep yielding to input');
  }
  assert.deepEqual(errors,[]);
} finally {
  await browser?.close();server?.kill();
  await Promise.all([`${name}.html`,`${name}.tsx`].map(path=>rm(path,{force:true})));
}
