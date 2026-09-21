import assert from 'node:assert/strict';
import {writeFile,rm,access} from 'node:fs/promises';
import {spawn} from 'node:child_process';
import {chromium} from 'playwright-core';
import {visibleGroupRange,groupAtY} from '../src/transcriptViewport.ts';
assert.deepEqual(visibleGroupRange([24,264,504,744],264,100),{start:0,end:2});
assert.equal(groupAtY([24,264,504],264),1);
const name = 'long-session-check-'+process.pid;
let server,browser;
try {
 await writeFile(name+'.html','<div id="root"></div><script type="module" src="/'+name+'.tsx"></script>');
 await writeFile(name+'.tsx',`
import {render} from 'solid-js/web';
import {createSignal} from 'solid-js';
import {CanvasTranscript} from './src/components/CanvasTranscript';
import './src/app.css';
const [session,setSession]=createSignal({id:'empty',groups:[]});
let handle,totalChars=0,reads=new Set(),measures=0,paints=0,maxGap=0,last=performance.now(),frames=0;
const decodedImages=[];
window.Image=new Proxy(window.Image,{construct(target,args){const image=Reflect.construct(target,args);decodedImages.push(image);return image}});
const measure=CanvasRenderingContext2D.prototype.measureText;
CanvasRenderingContext2D.prototype.measureText=function(text){measures++;return measure.call(this,text)};
const fill=CanvasRenderingContext2D.prototype.fillText;
CanvasRenderingContext2D.prototype.fillText=function(...args){paints++;return fill.apply(this,args)};
function tick(now){maxGap=Math.max(maxGap,now-last);last=now;frames++;requestAnimationFrame(tick)}
requestAnimationFrame(tick);
window.load=(id,count)=>{
 const texts=Array.from({length:count},(_,i)=>'reply-'+i+' 中文检查 **bold** and code '+('text '+i+' ').repeat(140));
 totalChars=texts.reduce((sum,text)=>sum+text.length,0);
 const groups=Array.from({length:count},(_,i)=>({user:{type:'user',id:i*3,text:'round-'+i,ts:0},
 body:[{type:'assistant',id:i*3+1,ts:0,get text(){reads.add(i);return texts[i]}}],
 turn:{type:'turn',id:i*3+2,ts:0,durationMs:1000,stopReason:'end'}}));
 reads.clear();measures=paints=frames=maxGap=0;last=performance.now();
 setSession({id,groups});
};
window.jump=i=>handle.scrollToGroup(i);
window.bottom=()=>handle.scrollToBottom();
window.loadImages=count=>{
 const images=Array.from({length:count},(_,i)=>({name:'shot-'+i,mimeType:'image/svg+xml',
  data:btoa('<svg xmlns="http://www.w3.org/2000/svg" width="1000" height="600"><text x="20" y="30">shot '+i+'</text></svg>')}));
 setSession({id:'screenshots',groups:[{user:{type:'user',id:1,text:'screenshots',images,ts:0},body:[],turn:{type:'turn',id:2,ts:0,stopReason:'end'}}]});
};
window.imageStats=()=>({created:decodedImages.length,live:decodedImages.filter(image=>image.getAttribute('src')).length});
window.topImages=()=>handle.scrollBy(-handle.maxScrollTop());
let pageReads=[];
window.loadPaged=()=>{
 pageReads=[];
 setSession({id:'paged',groups:Array.from({length:1000},(_,i)=>({
  user:{type:'user',id:i*3,text:'prompt '+i,ts:0},
  body:[{type:'assistant',id:i*3+1,ts:0,text:i>970?'last answer':'',deferred:i<=970}],
  turn:{type:'turn',id:i*3+2,ts:0,durationMs:1,stopReason:'end'}}))});
};
const loadItems=async ids=>{
 const id=session().id; pageReads.push(...ids);
 await new Promise(resolve=>setTimeout(resolve,40));
 if(session().id!==id)return;
 const selected=new Set(ids);
 setSession(s=>({...s,groups:s.groups.map(g=>g.body.some(item=>selected.has(item.id))
  ?{...g,body:g.body.map(item=>selected.has(item.id)?{...item,deferred:false,text:'hydrated '+item.id+' content '.repeat(100)}:item)}:g)}));
};
window.pageStats=()=>({reads:pageReads,remaining:session().groups.filter(g=>g.body.some(item=>item.deferred)).length});
window.stats=()=>({totalChars,reads:[...reads],measures,paints,maxGap,frames,top:handle.scrollTop(),max:handle.maxScrollTop(),active:handle.activeGroup()});
render(()=><div style="height:700px;display:flex"><CanvasTranscript ref={v=>handle=v} threadId={session().id}
 groups={session().groups} loadItems={loadItems} permissions={[]} running={false} loading={false} preview={false} onReturnToCurrent={()=>{}} emptyHint="ready"/></div>,document.getElementById('root'));
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

  await page.goto(`http://127.0.0.1:${port}/${name}.html`,{waitUntil:'domcontentloaded',timeout:60000});
  await page.waitForFunction(()=>typeof window.load==='function',null,{timeout:60000});
  const results=[];
  for(const count of [2000,10000,2000]){
    await page.evaluate(count=>window.load('session-'+count+'-'+Date.now(),count),count);
    await page.waitForFunction(count=>window.stats().reads.includes(count-1)&&window.stats().paints>0&&window.stats().max>0,count,{timeout:10000});
    await page.waitForTimeout(200);
    const stats=await page.evaluate(()=>window.stats());
    assert.ok(stats.reads.length<30, 'Screen-external history must not be parsed: '+stats.reads.length);
    assert.ok(stats.maxGap<500, 'Main thread blocked: '+stats.maxGap);
    assert.ok(Math.abs(stats.top-stats.max)<2,'Initial layout must stick to bottom');
    assert.ok(stats.totalChars>2000000);
    results.push({count,totalChars:stats.totalChars,readGroups:stats.reads.length,measures:stats.measures,maxGap:stats.maxGap});
    for(const index of [0,Math.floor(count/2),count-1]){
      await page.evaluate(i=>window.jump(i),index);
      await page.waitForFunction(i=>window.stats().reads.includes(i),index,{timeout:5000});
      await page.waitForTimeout(100);
      const jumped=await page.evaluate(()=>window.stats());
      if (index < count-1) assert.equal(jumped.active,index,'Jump anchor changed: '+JSON.stringify({index,active:jumped.active}));
      else assert.ok(jumped.max-jumped.top<2,'Last short group must be visible at the bottom');
    }
    await page.evaluate(i=>window.jump(i),Math.floor(count/2));
    await page.waitForTimeout(100);
    await page.setViewportSize({width:1050,height:800});
    await page.waitForTimeout(250);
    assert.equal(await page.evaluate(()=>window.stats().active),Math.floor(count/2),'Resize must retain the reading anchor');
    await page.setViewportSize({width:1280,height:800});
    await page.evaluate(()=>window.bottom());
    await page.waitForTimeout(150);
    assert.equal(await page.evaluate(()=>Math.abs(window.stats().top-window.stats().max)<2),true);
  }
  // A queued old viewport layout must not overwrite a newer session.
  await page.evaluate(()=>{window.jump(0);window.load('replacement',3)});
  await page.waitForTimeout(250);
  assert.ok(await page.evaluate(()=>window.stats().active<3));
  await page.setViewportSize({width:1000,height:800});
  await page.waitForTimeout(250);
  await page.evaluate(()=>window.loadImages(200));
  await page.waitForTimeout(1000);
  const bottomImages=await page.evaluate(()=>window.imageStats());
  assert.ok(bottomImages.created>0&&bottomImages.created<80, 'A single round must not decode every screenshot: '+JSON.stringify(bottomImages));
  assert.ok(bottomImages.live<30, 'Decoded image cache must follow viewport: '+JSON.stringify(bottomImages));
  await page.evaluate(()=>window.topImages());
  await page.waitForTimeout(1000);
  const topImages=await page.evaluate(()=>window.imageStats());
  assert.ok(topImages.created>bottomImages.created&&topImages.live<30, 'Scroll must load new screenshots and release old ones: '+JSON.stringify(topImages));
  results.push({screenshots:200,bottomImages,topImages});
  await page.evaluate(()=>window.loadPaged());
  await page.waitForTimeout(200);
  assert.equal((await page.evaluate(()=>window.pageStats())).reads.length,0,'Latest page must not load old bodies');
  await page.evaluate(()=>window.jump(500));
  await page.waitForFunction(()=>window.pageStats().reads.includes(1501)&&window.pageStats().remaining<971);
  await page.waitForTimeout(300);
  assert.equal(await page.evaluate(()=>window.stats().active),500,'Hydration must preserve timeline/scroll anchor');
  assert.ok((await page.evaluate(()=>window.pageStats())).remaining>940,'Only viewport pages should load');
  assert.deepEqual(errors,[]);
  console.log(JSON.stringify(results));
} finally {
 await browser?.close();server?.kill();
 await Promise.all([name+'.html',name+'.tsx'].map(path=>rm(path,{force:true})));
}
