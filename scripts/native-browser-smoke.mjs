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
          if(command.args.method==='Page.captureScreenshot') {
            chromeCaptures.push(command.args.params);
            assert.equal(command.args.params.format,'webp');
            assert.equal(command.args.params.quality,100,'Chrome WebP capture must be lossless');
          }
          result=await cdp.send(command.args.method,command.args.params);
          if(command.args.method==='Page.captureScreenshot') {
            const bytes=Buffer.from(result.data,'base64');
            assert.equal(bytes.toString('ascii',0,4),'RIFF');
            assert.equal(bytes.toString('ascii',8,12),'WEBP');
            const chunks=[];
            for(let p=12;p+8<=bytes.length;) {
              chunks.push(bytes.toString('ascii',p,p+4));
              const size=bytes.readUInt32LE(p+4);p+=8+size+(size&1);
            }
            assert(chunks.includes('VP8L')&&!chunks.includes('VP8 '),'Chrome must return a lossless WebP bitstream');
          }
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
    // Reproduce models filling screenshot/action fields on an inspect call. It must return
    // the page directly, even when those unrelated values are non-empty or invalid.
    const chromeObs=await chromeUi('inspect',{tabTag:tag,query:'订单号',scope:'all',visual:'none',
      region:{x:0,y:0,width:0,height:0},ref:'placeholder',frame:99,fullPage:true,expandScroll:true,tileOffset:999,
      feedback:'unused',observe:'unused',settleMs:99999,
      action:{action:'fill',ref:'placeholder',text:'must not run'},expression:'throw Error("must not run")'});
    assert.ok(chromeObs.snapshotId,JSON.stringify(chromeObs));
    assert.equal(chromeObs.error,undefined);
    assert.equal(chromeCaptures.length,0,'DOM inspect must not turn into a screenshot');
    assert.equal(await page.locator('input').inputValue(),'','inspect must not execute unrelated actions');
    const conflict=await chromeUi('screenshot',{tabTag:tag,snapshotId:chromeObs.snapshotId,
      region:{x:0,y:0,width:100,height:100},ref:'placeholder'});
    assert.equal(conflict.status,'not_executed');
    assert.equal(conflict.inputAttempted,false);
    assert.match(conflict.reason,/不能同时提供/);
    assert.deepEqual(conflict.nextCall,{operation:'inspect',tabTag:tag});
    assert.equal(chromeCaptures.length,0,'conflicting screenshot crops must still fail before capture');
    const full=JSON.parse(await readFile(chromeObs.documentPath,'utf8'));
    const input=full.pages[0].items.find(i=>i.name==='订单号');assert(input);
    const changed=await chromeUi('act',{tabTag:tag,snapshotId:chromeObs.snapshotId,action:{action:'fill',frame:0,ref:input.ref,text:'Chrome桥接验证'}});
    assert.equal(changed.status,'executed');assert.ok(changed.snapshotId);
    assert.equal(await page.evaluate(()=>document.querySelector('input').value),'Chrome桥接验证');
    // A popup appearing after observation must refuse input and show enough context to recover.
    const button=full.pages[0].items.find(i=>i.name==='查询');assert(button);
    await page.evaluate(()=>document.body.insertAdjacentHTML('beforeend','<div id="blocker" role="dialog" style="position:fixed;inset:0;background:white;z-index:99999">Search suggestions<button onclick="this.parentElement.remove()">Dismiss suggestions</button></div>'));
    const blocked=await chromeUi('act',{tabTag:tag,snapshotId:changed.snapshotId,feedback:'inspect',action:{action:'click',ref:button.ref}});
    assert.equal(blocked.status,'not_executed');assert.equal(blocked.inputAttempted,false);
    assert.match(blocked.reason,/目标被遮挡/);assert.ok(blocked.images?.length,'occlusion feedback must include the actual screen');
    assert.ok(blocked.pages.every(p=>!p.delta),'failure must return full context, not an empty delta');
    assert.ok(blocked.pages.some(p=>p.elements.includes('Dismiss suggestions')));
    const dismissal=JSON.parse(await readFile(blocked.documentPath,'utf8')).pages[0].items.find(i=>i.name==='Dismiss suggestions');
    const recovered=await chromeUi('act',{tabTag:tag,snapshotId:blocked.snapshotId,feedback:'inspect',action:{action:'click',ref:dismissal.ref}});
    assert.equal(recovered.status,'executed');assert.equal(await page.locator('#blocker').count(),0);
    // A batch owns focus only when it clicked/filled that exact node. An old snapshot
    // ID is harmless for those live refs; focus theft and partial batches still stop.
    await page.setContent('<input id="first" aria-label="First"><input id="second" aria-label="Second"><output></output>');
    await page.evaluate(()=>{
      window.clicks=0;window.enters=0;
      document.querySelector('#first').focus();
      document.querySelector('#second').onclick=()=>window.clicks++;
      document.querySelector('#second').onkeydown=e=>{if(e.key==='Enter')window.enters++;};
    });
    const beforeFocus=await chromeUi('inspect',{tabTag:tag,visual:'none'});
    const focusedFields=JSON.parse(await readFile(beforeFocus.documentPath,'utf8')).pages[0].items;
    const second=focusedFields.find(i=>i.name==='Second');assert(second);
    const typed=await chromeUi('act',{tabTag:tag,snapshotId:beforeFocus.snapshotId,feedback:'inspect',actions:[
      {action:'click',ref:second.ref},{action:'type',text:'one batch'},{action:'press',key:'Enter'}]});
    assert.equal(typed.status,'executed',JSON.stringify(typed));
    assert.equal(typed.completedActions,3);assert.equal(await page.locator('#second').inputValue(),'one batch');
    const staleKeys=await chromeUi('act',{tabTag:tag,snapshotId:beforeFocus.snapshotId,action:{action:'press',key:'Enter'}});
    assert.equal(staleKeys.status,'not_executed');assert.equal(staleKeys.inputAttempted,false);
    const staleDom=await chromeUi('act',{tabTag:tag,snapshotId:beforeFocus.snapshotId,feedback:'inspect',actions:[
      {action:'fill',ref:second.ref,text:'same live node'},{action:'press',key:'Enter'}]});
    assert.equal(staleDom.status,'executed',JSON.stringify(staleDom));assert.equal(staleDom.completedActions,2);
    assert.equal(await page.locator('#second').inputValue(),'same live node');
    assert.equal(await page.evaluate(()=>window.enters),2,'stale snapshot recovery must not replay Enter');
    await page.evaluate(()=>{document.querySelector('#second').onclick=()=>{window.clicks++;setTimeout(()=>document.querySelector('#first').focus(),100);};});
    const stolen=await chromeUi('act',{tabTag:tag,snapshotId:staleDom.snapshotId,feedback:'inspect',actions:[
      {action:'click',ref:second.ref},{action:'wait',ms:250},{action:'type',text:'must not leak'}]});
    assert.equal(stolen.status,'needs_review');assert.equal(stolen.completedActions,2);assert.match(stolen.reason,/焦点/);
    assert.equal(await page.locator('#first').inputValue(),'');
    assert.equal(await page.locator('#second').inputValue(),'same live node');
    assert.equal(await page.evaluate(()=>window.clicks),3,'a partially completed batch must never be replayed');
    const stolenKey=await chromeUi('act',{tabTag:tag,snapshotId:stolen.snapshotId,actions:[
      {action:'click',ref:second.ref},{action:'wait',ms:250},{action:'press',key:'Enter'}]});
    assert.equal(stolenKey.status,'needs_review');assert.equal(stolenKey.completedActions,2);assert.match(stolenKey.reason,/焦点/);
    assert.equal(await page.evaluate(()=>window.enters),2);
    await page.evaluate(()=>{const e=document.querySelector('#second');e.replaceWith(e.cloneNode(true));});
    const replaced=await chromeUi('act',{tabTag:tag,snapshotId:stolen.snapshotId,action:{action:'fill',ref:second.ref,text:'wrong node'}});
    assert.equal(replaced.status,'not_executed');assert.equal(replaced.inputAttempted,false);assert.match(replaced.reason,/失效/);
    assert.equal(await page.locator('#second').inputValue(),'same live node');
    const fieldShot=await chromeUi('screenshot',{tabTag:tag,fullPage:false,maxEdge:0});
    const fieldRect=await page.locator('#second').boundingBox();
    const coordinateType=await chromeUi('act',{tabTag:tag,snapshotId:fieldShot.snapshotId,feedback:'inspect',actions:[
      {action:'click_at',x:fieldRect.x+fieldRect.width/2,y:fieldRect.y+fieldRect.height/2},
      {action:'press',key:'Ctrl+A'},{action:'type',text:'coordinate focus'}]});
    assert.equal(coordinateType.status,'executed',JSON.stringify(coordinateType));
    assert.equal(await page.locator('#second').inputValue(),'coordinate focus');
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
    // Default full-page screenshots use the same merged 32px deltas, per document tile.
    await page.evaluate(()=>{
      document.body.style.cssText='margin:0;height:5000px;background:white';
      document.body.innerHTML='<div id="delta-top" style="position:absolute;left:64px;top:64px;width:64px;height:32px"></div><div id="delta-bottom" style="position:absolute;left:128px;top:4400px;width:32px;height:32px"></div>';
      scrollTo(0,0);
    });
    const fullKey=await chromeUi('screenshot',{tabTag:tag,maxEdge:0});
    assert.equal(fullKey.fullPage,true);
    assert.equal(fullKey.totalTiles,2);
    assert(fullKey.images.every(i=>i.keyframe===true && i.frameStream.includes(':document:')));
    await page.evaluate(()=>{document.querySelector('#delta-top').style.background='black';document.querySelector('#delta-bottom').style.background='black';});
    const fullDelta=await chromeUi('screenshot',{tabTag:tag,maxEdge:0});
    assert.equal(fullDelta.totalTiles,2);assert.equal(fullDelta.nextTile,null);assert.equal(fullDelta.screenshotComplete,true);
    const patches=fullDelta.images.filter(i=>i.deltaOf);
    assert(patches.length>=2,JSON.stringify(fullDelta));
    assert(patches.every(i=>i.pixelWidth<=96&&i.pixelHeight<=64),'distant document changes must not become one bounding box');
    for(const base of fullDelta.images.filter(i=>!i.deltaOf)) {
      assert.equal(base.path,undefined);assert.equal(base.frameGroup,fullKey.snapshotId);
    }
    const fullSame=await chromeUi('screenshot',{tabTag:tag,maxEdge:0});
    assert(fullSame.images.every(i=>i.unchanged===true && !i.path));
    assert.equal(fullSame.screenshotComplete,true);
    // Script feedback consumes its input snapshot but retains a safe pixel-comparison baseline.
    const viewKey=await chromeUi('screenshot',{tabTag:tag,fullPage:false,maxEdge:0});
    const viewDelta=await chromeUi('evaluate',{tabTag:tag,snapshotId:viewKey.snapshotId,feedback:'screenshot',maxEdge:0,
      expression:'document.querySelector("#delta-top").style.background="red"; true'});
    assert.equal(viewDelta.status,'executed');
    assert.equal(viewDelta.images[0].frameGroup,viewKey.images[0].frameGroup);
    assert(viewDelta.images.some(i=>i.deltaOf),JSON.stringify(viewDelta));
    // expandScroll: an inner viewport-sized scroller (TAPD-like table) is captured in full, then restored.
    await page.evaluate(()=>{document.documentElement.style.overflow='hidden';document.body.style.cssText='';document.body.innerHTML='<div id="pane" style="position:fixed;top:40px;left:0;right:0;bottom:0;overflow:auto"><div style="height:3000px">rows</div></div>';});
    const viewportHeight=await page.evaluate(()=>innerHeight);
    const tall=await chromeUi('screenshot',{tabTag:tag,expandScroll:true,maxEdge:0});
    assert.equal(tall.scrollExpanded?.complete,true,JSON.stringify(tall.scrollExpanded));
    assert.ok(tall.documentSize.height>=3040,JSON.stringify(tall.documentSize));
    assert.equal(await page.evaluate(()=>innerHeight),viewportHeight,'viewport override must be cleared');
    const paneObs=await chromeUi('inspect',{tabTag:tag});
    const pane=JSON.parse(await readFile(paneObs.documentPath,'utf8')).pages[0].items.find(i=>i.scroll);
    assert(pane,'scroll container is observable');
    const region=await chromeUi('screenshot',{tabTag:tag,snapshotId:paneObs.snapshotId,ref:pane.ref,expandScroll:true});
    assert.match(region.images[0].path,/\.webp$/,'expandScroll keeps the original lossless image by default');
    assert.equal(region.scrollExpanded?.complete,true,JSON.stringify(region.scrollExpanded));
    assert.equal(region.images[0].y,40);assert.ok(region.documentSize.height>=3000,JSON.stringify(region.documentSize));
    assert.equal(await page.evaluate(()=>innerHeight),viewportHeight,'viewport override must be cleared');
    if(pollingError)throw pollingError;
    await writeFile(join(output,'coordinate-report.json'),JSON.stringify({passed:true,modelCalls:0,coordinateHover:true,localCrop:true,stalePixels:true,chromeCaptures,profile},null,2));
    console.log('PASS compiled Chrome bridge/inspect parameter isolation/screenshot crop validation/evaluate annotations, readback, stale snapshots, partial failure and Promise timeout/DOM/images/coordinate guards; no sidebar browser');
  }finally{abort.abort();await polling.catch(()=>{});}
} finally {
  await browser?.close();
  for (const socket of sockets) socket.close();
  if (child.exitCode === null && child.pid) spawnSync('taskkill',['/PID',String(child.pid),'/T','/F'],{windowsHide:true,stdio:'ignore'});
  await writeFile(join(output,'stderr.txt'),stderr);
}
