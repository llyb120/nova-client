// Real Windows Nova/WebView2 + Rust store/page/asset IPC, in a disposable data root.
// Build a CI-only debug copy with loopback WebView2 debugging at TEST_CDP_PORT.
// No provider credentials are needed or used. Sending/provider cancellation itself
// is covered at the UI boundary separately, not claimed by this storage smoke test.
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { randomUUID, createHash } from 'node:crypto';
import { deflateSync } from 'node:zlib';
import path from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { createServer } from 'vite';
import { chromium } from 'playwright-core';
if(process.platform!=='win32')throw Error('This smoke test requires Windows/WebView2');
const exe=path.resolve(process.argv[2]||'src-tauri/target/debug/nova.exe');
const root=await mkdtemp(path.join(tmpdir(),'nova-history-smoke-'));
const data=path.join(root,'data'),reportDir=path.resolve(process.env.TEST_REPORT_DIR||'history-native-results');
await mkdir(path.join(data,'threads'),{recursive:true});await mkdir(reportDir,{recursive:true});
const id=randomUUID(),smallId=randomUUID(),port=Number(process.env.TEST_CDP_PORT||9222);
const results=[],logs=[];let server,app,browser,page;
function png(width,height){
 const crc=b=>{let c=0xffffffff;for(const n of b){c^=n;for(let k=0;k<8;k++)c=(c>>>1)^((c&1)?0xedb88320:0);}return (c^0xffffffff)>>>0;};
 const chunk=(name,bytes)=>{const type=Buffer.from(name),out=Buffer.alloc(12+bytes.length);out.writeUInt32BE(bytes.length);type.copy(out,4);bytes.copy(out,8);out.writeUInt32BE(crc(Buffer.concat([type,bytes])),8+bytes.length);return out;};
 const ihdr=Buffer.alloc(13);ihdr.writeUInt32BE(width);ihdr.writeUInt32BE(height,4);ihdr[8]=8;ihdr[9]=2;
 const raw=Buffer.alloc((width*3+1)*height);
 for(let y=0;y<height;y++)for(let x=0;x<width;x++){const n=y*(width*3+1)+1+x*3;raw[n]=(x>>3)%256;raw[n+1]=(y>>2)%256;raw[n+2]=170;}
 return Buffer.concat([Buffer.from([137,80,78,71,13,10,26,10]),chunk('IHDR',ihdr),chunk('IDAT',deflateSync(raw)),chunk('IEND',Buffer.alloc(0))]);
}
const original=png(1920,1080),b64=original.toString('base64'),hash=createHash('sha256').update(original).digest('hex');
const items=[];
for(let round=0;round<1200;round++){
 const n=round*4;
 items.push({type:'user',id:n+1,ts:Date.now(),text:`native-prompt-${round}`,images:round%3===0?[{name:'original.png',mimeType:'image/png',data:b64,size:original.length}]:[]});
 items.push({type:'assistant',id:n+2,ts:Date.now(),text:`native-reply-${round}\n`+'A result with history and real image data. '.repeat(35)});
 items.push({type:'tool',id:n+3,ts:Date.now(),toolCallId:`shot-${round}`,title:'native picture',kind:'read',status:'completed',content:round%3===0?[{type:'image',data:b64,mimeType:'image/png'}]:[],locations:[]});
 items.push({type:'turn',id:n+4,ts:Date.now(),durationMs:1000,totalTokens:500,inputTokens:350,outputTokens:150,stopReason:'end'});
}
const header=threadId=>({id:threadId,title:threadId===id?'Native image history':'Small history',cwd:root,agentKind:'devin',createdAt:Date.now(),updatedAt:Date.now(),ephemeral:false,starred:false});
await writeFile(path.join(data,'threads',id+'.json'),JSON.stringify({...header(id),items}));
await writeFile(path.join(data,'threads',smallId+'.json'),JSON.stringify({...header(smallId),items:items.slice(-4)}));
async function start(){
 app=spawn(exe,[],{env:{...process.env,NOVA_DATA_DIR:data,WEBVIEW2_USER_DATA_FOLDER:path.join(root,'webview'),WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:`--remote-debugging-port=${port}`},stdio:['ignore','pipe','pipe']});
 app.on('error',e=>logs.push(String(e)));for(const s of [app.stdout,app.stderr])s.on('data',b=>logs.push(b.toString()));
 let error;
 for(let attempt=0;attempt<180;attempt++){
  if(app.exitCode!==null)throw Error(`Nova exited ${app.exitCode}: ${logs.join('')}`);
  try {const r=await fetch(`http://127.0.0.1:${port}/json/version`,{signal:AbortSignal.timeout(1000)});if(r.ok){browser=await chromium.connectOverCDP(`http://127.0.0.1:${port}`);break;}}catch(e){error=e;}
  await delay(500);
 }
 assert.ok(browser,`WebView2 connection: ${error}`);page=browser.contexts()[0].pages()[0];page.setDefaultTimeout(30000);
 page.on('pageerror',e=>logs.push('WebView error: '+String(e.stack||e)));
 await page.waitForFunction(()=>!!window.__TAURI__?.core);
 await page.evaluate(async()=>{window.historySmoke={store:await import('/src/store.ts'),calls:[]};const invoke=window.__TAURI_INTERNALS__.invoke;
  window.__TAURI_INTERNALS__.invoke=async(command,args,options)=>{const t=performance.now();try{return await invoke(command,args,options);}finally{window.historySmoke.calls.push({command,ms:performance.now()-t});}};});
 await page.waitForFunction(()=>!!window.historySmoke.store.state.settings);
}
async function stop(){
 await browser?.close().catch(()=>{});browser=null;
 if(app?.pid&&app.exitCode===null)spawnSync('taskkill.exe',['/PID',String(app.pid),'/T','/F'],{timeout:15000,windowsHide:true});
 app=null;await delay(500);
}
async function check(name,fn){const started=performance.now();const metrics=await fn();results.push({name,result:'passed',elapsedMs:performance.now()-started,...metrics});console.log('PASS '+name);}
try {
 server=await createServer({server:{host:'127.0.0.1',port:5173,strictPort:true}});await server.listen();await start();
 await check('actual app opens 1,200 rounds / 800 embedded originals through a bounded page',async()=>{
  const metrics=await page.evaluate(async threadId=>{const t=performance.now();await window.historySmoke.store.openThread(threadId);const s=window.historySmoke.store.state;
   return {openMs:performance.now()-t,items:s.items.length,bytes:new TextEncoder().encode(JSON.stringify(s.items)).length,total:s.history.totalItems,calls:window.historySmoke.calls};},id);
  assert.equal(metrics.total,4800);assert.ok(metrics.items<=80);assert.ok(metrics.bytes<=512*1024);assert.ok(!metrics.calls.some(c=>c.command==='get_thread'||c.command==='get_time_machine_timeline'));
  await page.waitForFunction(()=>document.querySelector('canvas.transcript-canvas-only')?.height>0);await delay(500);return metrics;
 });
 await check('real native paging retains all item identities and bounds the frontend window',async()=>{
  const snapshots=await page.evaluate(async()=>{const store=window.historySmoke.store,out=[];for(let i=0;i<5;i++){await store.loadHistoryPage('before');out.push({start:store.state.history.start,end:store.state.history.end,items:store.state.items.length});}return out;});
  assert.ok(snapshots.every(s=>s.items<=240));assert.ok(snapshots.at(-1).start<snapshots[0].start);return {snapshots};
 });
 await check('real thumbnail IPC resolves original identity without changing original pixels',async()=>{
  const result=await page.evaluate(async threadId=>{const invoke=window.__TAURI__.core.invoke;
   const p=await invoke('get_thread_page',{threadId,request:{aroundId:1}});const reference=p.thread.items.find(i=>i.id===1).images[0].uri;
   const thumb=await invoke('get_history_image',{reference,maxEdge:480,original:false});
   const original=await invoke('get_history_image',{reference,maxEdge:480,original:true});
   const asset=await import('/src/historyImages.ts');const image=new Image();image.src=asset.assetUrl(thumb.thumbnailUri||thumb.uri);await image.decode();
   return {thumb,original,decodedWidth:image.naturalWidth,decodedHeight:image.naturalHeight};},id);
  assert.equal(result.original.attachmentId,hash);assert.equal(result.original.width,1920);assert.equal(result.original.height,1080);assert.ok(result.decodedWidth<=480);assert.ok(result.decodedHeight<=480);return result;
 });
 await check('lightweight cancellation command and session switch coexist with page requests',async()=>{
  const metrics=await page.evaluate(async ids=>{const invoke=window.__TAURI__.core.invoke;const loads=Array.from({length:4},()=>invoke('get_thread_page',{threadId:ids.id,request:{limit:80}}));
   const started=performance.now();await invoke('cancel_turn',{threadId:ids.id});const cancelMs=performance.now()-started;await window.historySmoke.store.openThread(ids.smallId);await Promise.all(loads);
   return {cancelMs,current:window.historySmoke.store.state.currentId,items:window.historySmoke.store.state.items.length};},{id,smallId});
  assert.equal(metrics.current,smallId);assert.equal(metrics.items,4);assert.ok(metrics.cancelMs<5000);return metrics;
 });
 await check('background migration publishes a verified chunk manifest and preserves the legacy backup',async()=>{
  let manifest;
  for(let i=0;i<120;i++){try{manifest=JSON.parse(await readFile(path.join(data,'threads',id+'.json'),'utf8'));if(manifest.format==='nova-history-chunks-v1')break;}catch{}await delay(500);}
  assert.equal(manifest?.format,'nova-history-chunks-v1');assert.equal(manifest.item_count,4800);
  const old=JSON.parse(await readFile(path.join(data,'threads',id+'.json.pre-chunks'),'utf8'));assert.equal(old.items.length,4800);assert.equal(old.items[0].images[0].data,b64);
  return {chunks:manifest.chunks.length,sourceImageBytes:original.length,sourceItems:items.length};
 });
 await page.screenshot({path:path.join(reportDir,'native-history.png')});await stop();await start();
 await check('restarting from migrated chunks preserves history, original assets, and aggregate tokens',async()=>{
  const result=await page.evaluate(async threadId=>{await window.historySmoke.store.openThread(threadId);const s=window.historySmoke.store.state;return {total:s.history.totalItems,users:s.history.stats.users,tokens:s.history.stats.totalTokens};},id);
  assert.deepEqual(result,{total:4800,users:1200,tokens:600000});return result;
 });
} catch(error){results.push({result:'failed',error:String(error.stack||error)});await page?.screenshot({path:path.join(reportDir,'failure.png')}).catch(()=>{});throw error;}
finally {await writeFile(path.join(reportDir,'report.json'),JSON.stringify({scope:'real Windows app / store / page / image IPC; no external provider',results},null,2));await writeFile(path.join(reportDir,'app.log'),logs.join('\n'));await stop();await server?.close();await rm(root,{recursive:true,force:true,maxRetries:3}).catch(()=>{});}
