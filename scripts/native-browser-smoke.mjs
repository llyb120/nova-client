// Windows integration check: isolated Nova + Chromium fixture + emulated Chrome extension.
// Playwright drives only the test fixture; all tested operations go through Nova's Chrome bridge.
// No shared browser/profile, model or external site is used. This is not a real extension E2E test.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { spawn, spawnSync } from 'node:child_process';
import { mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { resolve, join } from 'node:path';
import { chromium } from 'playwright-core';

const root = process.cwd();
const executable = resolve(process.argv[2] || 'src-tauri/target/debug/nova.exe');
const profile = await mkdtemp(join(tmpdir(), 'nova-chrome-smoke-'));
const output = resolve('src-tauri/target/native-browser-smoke');
await mkdir(output, { recursive: true });
await writeFile(join(profile,'settings.json'),JSON.stringify({relayServer:'',relayToken:'',sessionShortcuts:[],lyraEnabled:false}));
const portServer = createServer(); await new Promise(r => portServer.listen(0, '127.0.0.1', r));
const debugPort = portServer.address().port; await new Promise(r => portServer.close(r));
const child = spawn(executable, [], { cwd: root, windowsHide: true, env: { ...process.env, NOVA_CHROME_PORT: '0', NOVA_DATA_DIR: profile, WEBVIEW2_USER_DATA_FOLDER: join(profile,'app-runtime'), WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${debugPort} --remote-debugging-address=127.0.0.1 --disable-features=CalculateNativeWinOcclusion` }, stdio: ['ignore', 'ignore', 'pipe'] });
let stderr = '', spawnError;
child.stderr.on('data', d => { stderr += d; });
child.on('error', error => { spawnError = error; });
const sockets = [];
const sleep = ms => new Promise(r => setTimeout(r, ms));
let browser;
async function until(fn, label) {
  for (let i = 0; i < 150; i++) {
    if (spawnError) throw spawnError;
    if (child.exitCode !== null) throw Error(`Nova exited ${child.exitCode}: ${stderr.slice(-3000)}`);
    try { const value = await fn(); if (value) return value; } catch {}
    await sleep(100);
  }
  throw Error(`Timed out: ${label}`);
}
async function attach(target) {
  const socket = new WebSocket(target.webSocketDebuggerUrl); sockets.push(socket);
  await new Promise((r,j) => { socket.addEventListener('open',r,{once:true}); socket.addEventListener('error',j,{once:true}); });
  const pending = new Map(); let sequence = 0;
  socket.addEventListener('message', ({data}) => {
    const m = JSON.parse(data); const job = pending.get(m.id); if (!job) return;
    clearTimeout(job.timer); pending.delete(m.id); m.error ? job.reject(Error(JSON.stringify(m.error))) : job.resolve(m.result);
  });
  const call = (method, params = {}) => new Promise((resolve,reject) => { const id=++sequence; const timer=setTimeout(()=>{pending.delete(id);reject(Error(`${method} timed out`));},120000); pending.set(id,{resolve,reject,timer}); socket.send(JSON.stringify({id,method,params})); });
  const evaluate = async expression => { const v=await call('Runtime.evaluate',{expression,returnByValue:true,awaitPromise:true}); if(v.exceptionDetails)throw Error(JSON.stringify(v.exceptionDetails)); return v.result.value; };
  return { call, evaluate };
}
try {
  const mainTarget=await until(async()=> (await (await fetch(`http://127.0.0.1:${debugPort}/json/list`)).json()).find(t=>t.type==='page'&&!t.url.startsWith('devtools:')),'main');
  const main=await attach(mainTarget);
  await until(()=>main.evaluate('!!window.__TAURI_INTERNALS__&&!!document.querySelector(".app")'),'UI');
  const invoke=(command,args={})=>main.evaluate(`window.__TAURI_INTERNALS__.invoke(${JSON.stringify(command)},${JSON.stringify(args)})`);
  assert.equal(await main.evaluate('!!document.querySelector(".workspace-browser-surface")'),false);
  await assert.rejects(invoke('native_browser_ui',{threadId:'removed',operation:'mount',args:{}}),/not found|unknown/i);
  const thread=await invoke('create_thread',{cwd:root,agentKind:'lyra',model:'',mode:'build',ephemeral:false});
  await until(()=>main.evaluate('!!document.querySelector(".thread-item")'),'thread');
  await main.evaluate('document.querySelector(".thread-item").click()');
  await invoke('report_activity',{threadId:thread.id});

  browser=await chromium.launch({executablePath:process.env.TEST_BROWSER,headless:true,args:['--no-sandbox']});
  const context=await browser.newContext({viewport:{width:900,height:700}});
  const page=await context.newPage();
  await page.goto('data:text/html,'+encodeURIComponent('<!doctype html><meta charset="utf-8"><title>Chrome fixture</title><label>订单号<input></label><button onclick="document.querySelector(\'output\').textContent=document.querySelector(\'input\').value">查询</button><output></output>'));
  const cdp=await context.newCDPSession(page);
  const chromeUi=(operation,args={})=>invoke('chrome_browser_ui',{operation,args});
  const connection=await chromeUi('connect');
  const pairing=await fetch(connection.origin+'/pair',{method:'POST',headers:{Origin:`chrome-extension://${connection.extensionId}`}});
  assert.equal(pairing.status,200);
  const config={origin:connection.origin,...await pairing.json()};
  const abort=new AbortController();
  const tag='C1-smoketest';
  const chromeCaptures=[];
  const post=async(route,body)=>{
    const response=await fetch(config.origin+route,{method:'POST',headers:{'Content-Type':'application/json',Origin:`chrome-extension://${connection.extensionId}`,Authorization:`Bearer ${config.token}`},body:JSON.stringify({clientId:'12345678-1234-4234-8234-123456789abc',...body}),signal:abort.signal});
    assert.equal(response.status,200);return response.json();
  };
  const polling=(async()=>{
    while(!abort.signal.aborted){
      const {command}=await post('/poll',{});if(!command)continue;
      let reply;
      try {
        let result;
        if(command.operation==='tabs')result={tabs:[{tag,allowed:true,controllable:true,title:'Fixture',url:page.url()}]};
        else if(command.operation==='status')result={incognitoAllowed:false};
        else if(command.operation==='select_tab'){assert.equal(command.args.tabTag,tag);result={tabTag:tag};}
        else {
          assert.equal(command.args.tabTag,tag);assert.equal(command.operation,'cdp');
          assert(command.args.sessionId == null,'fixture has no cross-process iframe');
          if(command.args.method==='Page.captureScreenshot')chromeCaptures.push(command.args.params);
          result=await cdp.send(command.args.method,command.args.params);
        }
        reply={id:command.id,result};
      }catch(error){reply={id:command.id,error:String(error)};}
      await post('/reply',reply);
    }
  })();
  let pollingError;polling.catch(error=>{if(!abort.signal.aborted)pollingError=error;});
  try {
    await until(async()=>(await chromeUi('status')).connected,'Chrome bridge');
    assert.equal((await chromeUi('tabs')).tabs[0].tag,tag);
    await assert.rejects(chromeUi('inspect'),/tabTag/);
    const chromeObs=await chromeUi('inspect',{tabTag:tag});
    const full=JSON.parse(await readFile(chromeObs.documentPath,'utf8'));
    const input=full.pages[0].items.find(i=>i.name==='订单号');assert(input);
    const changed=await chromeUi('act',{tabTag:tag,snapshotId:chromeObs.snapshotId,action:{action:'fill',frame:0,ref:input.ref,text:'Chrome桥接验证'}});
    assert.equal(changed.status,'executed');assert.ok(changed.snapshotId);
    assert.equal(await page.evaluate(()=>document.querySelector('input').value),'Chrome桥接验证');
    const script=`(() => { const n=document.createElement('aside'); n.className='nova-req-annot'; n.textContent='待确认'; document.body.append(n); return {count:document.querySelectorAll('.nova-req-annot').length}; })()`;
    const missing=await chromeUi('evaluate',{tabTag:tag,expression:script});
    assert.equal(missing.status,'not_executed');assert.ok(missing.snapshotId);
    assert.equal(await page.locator('.nova-req-annot').count(),0);
    const annotated=await chromeUi('evaluate',{tabTag:tag,snapshotId:missing.snapshotId,expression:script,feedback:'screenshot'});
    assert.equal(annotated.status,'executed',JSON.stringify(annotated));assert.deepEqual(annotated.value,{count:1});
    assert.ok(annotated.images.length);assert.ok(annotated.snapshotId);
    const reused=await chromeUi('evaluate',{tabTag:tag,snapshotId:missing.snapshotId,expression:script});
    assert.equal(reused.status,'not_executed');assert.equal(await page.locator('.nova-req-annot').count(),1);
    const failed=await chromeUi('evaluate',{tabTag:tag,snapshotId:reused.snapshotId,expression:'document.querySelector(".nova-req-annot").dataset.checked="yes"; throw Error("partial script")'});
    assert.equal(failed.status,'needs_review');assert.match(failed.reason,/partial script/);
    assert.equal(await page.locator('.nova-req-annot').getAttribute('data-checked'),'yes');assert.ok(failed.snapshotId);
    const readback=await chromeUi('evaluate',{tabTag:tag,snapshotId:failed.snapshotId,expression:'Promise.resolve(document.querySelectorAll(".nova-req-annot").length)',feedback:'none'});
    assert.equal(readback.value,1);assert.equal(readback.snapshotId,undefined);
    const consumed=await chromeUi('evaluate',{tabTag:tag,snapshotId:failed.snapshotId,expression:script});
    assert.equal(consumed.status,'not_executed');assert.equal(await page.locator('.nova-req-annot').count(),1);
    const waiting=await chromeUi('evaluate',{tabTag:tag,snapshotId:consumed.snapshotId,expression:'new Promise(resolve => setTimeout(() => resolve(1), 6000))'});
    assert.equal(waiting.status,'needs_review');assert.match(waiting.reason,/5秒超时.*可能仍在运行/);
    assert.equal(waiting.inputAttempted,true);
    const screenshot=await chromeUi('screenshot',{tabTag:tag,fullPage:false});
    assert.ok(screenshot.images.length);assert.equal(screenshot.browser,'chrome');
    // Compiled pixel preflight, image mapping and post-hover checks remain covered.
    await page.evaluate(()=>document.body.insertAdjacentHTML('beforeend','<style>#hover-option{position:fixed;left:300px;top:200px;width:200px;height:40px;background:white;z-index:99999}#hover-option:hover{background:rgb(40,150,240)}</style><div id="hover-option" onclick="window.optionClicked=(window.optionClicked||0)+1">Leave-Pay</div>'));
    for(const deviceScaleFactor of [1,1.25,2]) {
      await cdp.send('Emulation.setDeviceMetricsOverride',{width:900,height:700,deviceScaleFactor,mobile:false});
      for(const options of [{maxEdge:320},{maxEdge:0,region:{x:280.4,y:180.4,width:240.2,height:80.2}}]) {
        await cdp.send('Input.dispatchMouseEvent',{type:'mouseMoved',x:10,y:10});
        const geometry=await page.evaluate(()=>document.querySelector('#hover-option').getBoundingClientRect().toJSON());
        const shot=await chromeUi('screenshot',{tabTag:tag,fullPage:false,...options});
        assert.deepEqual(await page.evaluate(()=>document.querySelector('#hover-option').getBoundingClientRect().toJSON()),geometry);
        const image=shot.images[0], count=chromeCaptures.length;
        const clicked=await chromeUi('act',{tabTag:tag,snapshotId:shot.snapshotId,imageId:image.imageId,feedback:'screenshot',action:{action:'click_at',x:(340-image.x)*image.pixelWidth/image.width,y:(220-image.y)*image.pixelHeight/image.height}});
        assert.equal(clicked.status,'executed',JSON.stringify(clicked));
        assert.equal(chromeCaptures.length-count,2,'one preflight and one feedback; DOM hover does not need a second screenshot');
      }
    }
    await cdp.send('Emulation.clearDeviceMetricsOverride');
    assert.equal(await page.evaluate(()=>window.optionClicked),6);
    assert.ok(chromeCaptures.every(params=>!params.clip && params.fromSurface===true && params.captureBeyondViewport===false),'viewport captures must not override the browser render scale/clip');
    await cdp.send('Input.dispatchMouseEvent',{type:'mouseMoved',x:10,y:10});
    const stale=await chromeUi('screenshot',{tabTag:tag,fullPage:false});
    await page.evaluate(()=>{document.querySelector('#hover-option').style.background='black';});
    const refused=await chromeUi('act',{tabTag:tag,snapshotId:stale.snapshotId,action:{action:'click_at',x:340,y:220}});
    assert.equal(refused.status,'not_executed');assert.match(refused.reason,/画面已变化/);
    assert.equal(await page.evaluate(()=>window.optionClicked),6,'pre-existing visual changes still block input');
    // expandScroll: an inner viewport-sized scroller (TAPD-like table) is captured in full, then restored.
    await page.evaluate(()=>{document.documentElement.style.overflow='hidden';document.body.innerHTML='<div id="pane" style="position:fixed;top:40px;left:0;right:0;bottom:0;overflow:auto"><div style="height:3000px">rows</div></div>';});
    const viewportHeight=await page.evaluate(()=>innerHeight);
    const tall=await chromeUi('screenshot',{tabTag:tag,expandScroll:true,maxEdge:0});
    assert.equal(tall.scrollExpanded?.complete,true,JSON.stringify(tall.scrollExpanded));
    assert.ok(tall.documentSize.height>=3040,JSON.stringify(tall.documentSize));
    assert.equal(await page.evaluate(()=>innerHeight),viewportHeight,'viewport override must be cleared');
    const paneObs=await chromeUi('inspect',{tabTag:tag});
    const pane=JSON.parse(await readFile(paneObs.documentPath,'utf8')).pages[0].items.find(i=>i.scroll);
    assert(pane,'scroll container is observable');
    const region=await chromeUi('screenshot',{tabTag:tag,snapshotId:paneObs.snapshotId,ref:pane.ref,expandScroll:true});
    assert.match(region.images[0].path,/\.png$/,'expandScroll keeps the original lossless image by default');
    assert.equal(region.scrollExpanded?.complete,true,JSON.stringify(region.scrollExpanded));
    assert.equal(region.images[0].y,40);assert.ok(region.documentSize.height>=3000,JSON.stringify(region.documentSize));
    assert.equal(await page.evaluate(()=>innerHeight),viewportHeight,'viewport override must be cleared');
    if(pollingError)throw pollingError;
    await writeFile(join(output,'coordinate-report.json'),JSON.stringify({passed:true,modelCalls:0,coordinateHover:true,localCrop:true,stalePixels:true,chromeCaptures,profile},null,2));
    console.log('PASS compiled Chrome bridge/evaluate annotations, readback, stale snapshots, partial failure and Promise timeout/DOM/images/coordinate guards; no sidebar browser');
  }finally{abort.abort();await polling.catch(()=>{});}
} finally {
  await browser?.close();
  for (const socket of sockets) socket.close();
  if (child.exitCode === null && child.pid) spawnSync('taskkill',['/PID',String(child.pid),'/T','/F'],{windowsHide:true,stdio:'ignore'});
  await writeFile(join(output,'stderr.txt'),stderr);
}
