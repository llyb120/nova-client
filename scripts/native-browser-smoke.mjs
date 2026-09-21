// Windows integration check, real Nova + WebView2. Raw CDP is only the test driver;
// production browser operations use native COM. No Playwright. No model or auxiliary API is used.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { spawn, spawnSync } from 'node:child_process';
import { mkdir, mkdtemp, writeFile, readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { resolve, join } from 'node:path';

const root = process.cwd();
const coordinateOnly = process.argv.includes('--coordinates-only');
const executable = resolve(process.argv[2] || 'bench/webview-probe/target/debug/nova.exe');
const profile = await mkdtemp(join(tmpdir(), 'nova-browser-smoke-'));
const output = resolve('src-tauri/target/native-browser-smoke');
await mkdir(output, { recursive: true });
const server = createServer((req,res)=>{
  res.setHeader('Content-Type','text/html; charset=utf-8');
  if(req.url==='/frame') {res.end('<!doctype html><title>Frame fixture</title><button onclick="this.textContent=\'框架成功\'">框架按钮</button>');return;}
  res.end(`<!doctype html><meta charset="utf-8"><title>Browser fixture</title><style>body{font:16px system-ui;padding:20px}section{margin:20px 0;padding:12px;border:1px solid #888}input,button{padding:8px}::-webkit-scrollbar{width:16px}::-webkit-scrollbar-thumb{background:rgb(255,0,255)}.scrollbox{height:70px;overflow:scroll;scrollbar-gutter:stable}</style><h1>订单测试</h1><section><h2>客户筛选</h2><button onclick="window.wrong=true">查询</button></section><section><h2>订单筛选</h2><label>订单号<input oninput="window.trusted=event.isTrusted"></label><button onclick="document.querySelector('output').textContent=document.querySelector('input').value+' 已发货'">查询</button><output></output></section><div class="scrollbox"><div style="height:300px">滚动区域</div></div><div class="scrollbox" style="scrollbar-color:rgb(255,0,255) transparent"><div style="height:300px">标准滚动区域</div></div><iframe src="http://localhost:${server.address().port}/frame"></iframe><div style="height:600px">页尾</div>`);
});
await new Promise(r=>server.listen(0,'127.0.0.1',r));
const port=server.address().port;
await writeFile(join(profile,'settings.json'),JSON.stringify({relayServer:'',relayToken:'',sessionShortcuts:[],lyraEnabled:false}));
const portServer = createServer(); await new Promise(r => portServer.listen(0, '127.0.0.1', r));
const debugPort = portServer.address().port; await new Promise(r => portServer.close(r));
// Keep the isolated test renderer visible when another desktop window covers it during the 31s delay checks.
const child = spawn(executable, [], { cwd: root, windowsHide: true, env: { ...process.env, NOVA_CHROME_PORT: "0", NOVA_DATA_DIR: profile, WEBVIEW2_USER_DATA_FOLDER: join(profile,'webview-runtime'), WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${debugPort} --remote-debugging-address=127.0.0.1 --disable-features=CalculateNativeWinOcclusion` }, stdio: ['ignore', 'ignore', 'pipe'] });
let stderr = ''; child.stderr.on('data', d => { stderr += d; });
const sockets = [];
const sleep = ms => new Promise(r => setTimeout(r, ms));
async function until(fn, label) {
  for (let i = 0; i < 150; i++) { if (child.exitCode !== null) throw Error(`Nova exited ${child.exitCode}: ${stderr.slice(-3000)}`); try { const value = await fn(); if (value) return value; } catch {} await sleep(100); }
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
  const call = (method, params = {}, sessionId) => new Promise((resolve,reject) => { const id=++sequence; const timer=setTimeout(()=>{pending.delete(id);reject(Error(`${method} timed out`));},120000); pending.set(id,{resolve,reject,timer}); socket.send(JSON.stringify({id,method,params,...(sessionId?{sessionId}:{})})); });
  const evaluate = async expression => { const v=await call('Runtime.evaluate',{expression,returnByValue:true,awaitPromise:true}); if(v.exceptionDetails)throw Error(JSON.stringify(v.exceptionDetails)); return v.result.value; };
  return { call, evaluate };
}
const targets = async () => (await fetch(`http://127.0.0.1:${debugPort}/json/list`)).json();
try {
  const mainTarget=await until(async()=> (await targets()).find(t=>t.type==='page'&&!t.url.startsWith('devtools:')),'main');
  const main=await attach(mainTarget);
  await main.call('Emulation.setFocusEmulationEnabled',{enabled:true});
  await until(()=>main.evaluate('!!window.__TAURI_INTERNALS__&&!!document.querySelector(".app")'),'UI');
  const invoke=(command,args={})=>main.evaluate(`window.__TAURI_INTERNALS__.invoke(${JSON.stringify(command)},${JSON.stringify(args)})`);
  assert.equal('browserControlModel' in await invoke('get_settings'),false);
  await main.evaluate(`localStorage.setItem('fd:workspaceLayout',JSON.stringify({open:true,widthRatio:.6,minimap:true,softWrap:true,mode:'browser'}));location.reload()`);
  await until(()=>main.evaluate('!!document.querySelector(".app")'),'reload');
  await main.call('Emulation.setFocusEmulationEnabled',{enabled:true});
  const thread=await invoke('create_thread',{cwd:root,agentKind:'lyra',model:'',mode:'build',ephemeral:false});
  await until(()=>main.evaluate('!!document.querySelector(".thread-item")'),'thread');
  await main.evaluate('document.querySelector(".thread-item").click()');
  await invoke('report_activity',{threadId:thread.id});
  const ui=(operation,args={})=>invoke('native_browser_ui',{threadId:thread.id,operation,args});
  await until(async()=> (await ui('status')).visible,'visible browser');
  assert.equal(await main.evaluate('!!document.querySelector("[aria-label=浏览器局部目标]")'),false);
  await ui('goto',{url:`http://127.0.0.1:${port}/fixture`});
  const page=await attach(await until(async()=> (await targets()).find(t=>t.url.includes('/fixture')),'fixture'));
  await until(()=>page.evaluate('!!document.querySelector("input")'),'loaded');
  const timings=[];
  const act=async(action,snapshot)=>{
    // The isolated window may be occluded while the user works during the 31s delays.
    // Restore only the fixture surface; explicit UI hide/restore checks run separately below.
    if(!(await ui('status')).visible){
      const bounds=await main.evaluate('(()=>{const r=document.querySelector(".workspace-browser-surface").getBoundingClientRect();return {x:r.x,y:r.y,width:r.width,height:r.height}})()');
      await ui('layout',{visible:true,...bounds});
    }
    snapshot??=await ui('inspect');const start=performance.now();
    const result=await ui('act',{snapshotId:snapshot.snapshotId,action});timings.push(Math.round(performance.now()-start));return result;
  };
  const find=(observation,name,region)=>{
    for(const page of observation.pages){const item=page.items.find(i=>i.name===name&&(!region||i.region.includes(region)));if(item)return {frame:page.frame,ref:item.ref};}
    throw Error('Missing '+name+': '+JSON.stringify(observation));
  };
  let observation,summaryBytes,fullPageEvidence;
  if(!coordinateOnly) {
  observation=await ui('inspect');
  // Real inference/read-image delay must not expire a still-valid DOM reference.
  await sleep(31000);
  observation=await act({action:'fill',...find(observation,'订单号'),text:'订单-123'},observation);
  assert.equal(observation.status,'executed');assert.ok(observation.snapshotId);assert.ok(observation.durationMs>=observation.actionMs);
  assert.equal(observation.pages[0].items.find(i=>i.name==='订单号').value,'订单-123');
  assert.equal((await act({action:'click',...find(observation,'查询','订单筛选')},observation)).status,'executed');
  assert.equal(await page.evaluate('document.querySelector("output").textContent'),'订单-123 已发货');
  assert.equal(await page.evaluate('window.trusted'),true);assert.notEqual(await page.evaluate('window.wrong'),true);
  await page.evaluate(`document.body.insertAdjacentHTML('afterbegin','<a id="semantic-link" href="https://example.com/intelligence">情报导航</a><div role="menuitem" tabindex="0" aria-haspopup="menu" aria-expanded="false">区域菜单</div>');document.querySelector("[role=menuitem]").onclick=function(){this.setAttribute("aria-expanded","true")}`);
  observation=await ui('inspect',{query:'example.com/intelligence'});
  assert.equal(observation.pages[0].items[0].href,'https://example.com/intelligence');
  observation=await ui('inspect',{query:'区域菜单'});
  const feedback=await ui('act',{snapshotId:observation.snapshotId,action:{action:'click',...find(observation,'区域菜单')},feedback:'screenshot'});
  assert.equal(feedback.status,'executed');assert.equal(feedback.fullPage,false);assert.ok(feedback.images.length);
  assert.equal(feedback.pages[0].items.find(i=>i.name==='区域菜单').expanded,'true');
  await page.evaluate('document.querySelector("#semantic-link").remove();document.querySelector("[role=menuitem]").remove()');
  observation=await until(async()=>{const o=await ui('inspect');return o.pages.some(p=>p.items.some(i=>i.name==='框架按钮'))&&o;},'cross-origin frame ready');
  assert.equal((await act({action:'click',...find(observation,'框架按钮')},observation)).status,'executed');
  assert.ok(JSON.stringify(await ui('inspect')).includes('框架成功'));
  assert.equal((await act({action:'click',frame:0,ref:'invented'})).status,'not_executed');
  observation=await ui('inspect');
  await page.evaluate('const input=document.querySelector("input");input.replaceWith(input.cloneNode(true))');
  const replaced=await act({action:'fill',...find(observation,'订单号'),text:'不能写入替换元素'},observation);
  assert.equal(replaced.status,'not_executed');assert.match(replaced.reason,/引用已失效/);
  await page.evaluate(`scrollTo(0,0);document.body.insertAdjacentHTML('beforeend','<div id="shield" style="position:fixed;inset:0;background:#eee;z-index:9999"><button onclick="this.parentNode.remove()">关闭遮挡</button></div>')`);
  observation=await ui('inspect');
  const blocked=await act({action:'fill',...find(observation,'订单号'),text:'不应写入'},observation);
  assert.equal(blocked.status,'not_executed');assert.match(blocked.reason,/遮挡/);
  observation=await ui('inspect');await act({action:'click',...find(observation,'关闭遮挡')},observation);
  // Partial center occlusion should use a visible edge without an extra model or retry.
  await page.evaluate(`const r=document.querySelectorAll('button')[1].getBoundingClientRect();document.body.insertAdjacentHTML('beforeend','<div id="partial" style="position:fixed;left:'+(r.x+r.width*.4)+'px;top:'+r.y+'px;width:'+(r.width*.2)+'px;height:'+r.height+'px;background:red;z-index:9999"></div>')`);
  observation=await ui('inspect');assert.equal((await act({action:'click',...find(observation,'查询','订单筛选')},observation)).status,'executed');
  await page.evaluate('document.querySelector("#partial").remove();scrollTo(0,0)');
  const geometry=()=>page.evaluate('JSON.stringify({width:innerWidth,client:document.documentElement.clientWidth,field:document.querySelector("input").getBoundingClientRect().toJSON(),scroll:scrollY,nested:document.querySelector(".scrollbox").clientWidth})');
  const beforeGeometry=await geometry();
  observation=await ui('screenshot',{fullPage:false});assert.ok(observation.path);assert.equal(await geometry(),beforeGeometry);
  assert.equal(await page.evaluate('getComputedStyle(document.documentElement,"::-webkit-scrollbar-thumb").backgroundColor'),'rgb(255, 0, 255)');
  const field=observation.pages[0].items.find(i=>i.name==='订单号');
  await sleep(31000); // Screenshot + model reasoning used to exceed the old 30-second deadline.
  assert.equal((await act({action:'click_at',x:field.point.x,y:field.point.y},observation)).status,'executed');
  await assert.rejects(act({action:'click_at',x:field.point.x,y:field.point.y},observation),/inspect|失效/);
  await act({action:'press',key:'Control+A'});await act({action:'type',text:'保留表单'});
  // Stop interrupts a pending wait, leaving no model/process jobs behind.
  const waitObs=await ui('inspect');const wait=act({action:'wait',ms:2000},waitObs);await sleep(80);await ui('stop');await assert.rejects(wait,/停止/);
  // Read the whole loaded DOM without scrolling, including nested overflow and targets beyond the old 80/160 cap.
  await page.evaluate(`const section=document.createElement('section');section.id='whole-page';section.innerHTML=Array.from({length:350},(_,i)=>'<button>目标'+i+'</button>').join('')+'<div style="height:5000px">整页长内容</div><button id="bottom" onclick="window.bottomClicked=true" style="background:lime">整页底部按钮</button>';document.body.append(section);document.querySelector('.scrollbox>div').innerHTML+='<p style="margin-top:200px">内部滚动区域末尾文本</p>';scrollTo(0,0)`);
  const full=await ui('inspect');
  summaryBytes=Buffer.byteLength(JSON.stringify(full));
  assert.ok(full.pages.reduce((n,p)=>n+p.items.length,0)<=60);assert.ok(summaryBytes<40000,`summary: ${summaryBytes} bytes`);
  assert.equal(await page.evaluate('scrollY'),0);assert.equal(full.pages[0].inlineTruncated,true);
  const document=JSON.parse(await readFile(full.documentPath,'utf8'));
  assert.ok(document.pages[0].items.length>350);assert.ok(document.pages[0].text.includes('内部滚动区域末尾文本'));
  assert.ok(document.pages[0].text.includes('整页底部按钮'));assert.ok(document.pages[0].items.find(i=>i.name==='整页底部按钮'));
  const searched=await ui('inspect',{query:'整页底部按钮'});assert.ok(searched.pages[0].items.find(i=>i.name==='整页底部按钮'));
  const beforeFullGeometry=await page.evaluate('({scroll:scrollY,width:document.documentElement.clientWidth,bottom:document.querySelector("#bottom").getBoundingClientRect().y,gutter:document.documentElement.style.scrollbarGutter})');
  const fullShot=await ui('screenshot');assert.equal(fullShot.fullPage,true);assert.equal(fullShot.screenshotComplete,true);assert.ok(fullShot.images.length>1);
  assert.deepEqual(await page.evaluate('({scroll:scrollY,width:document.documentElement.clientWidth,bottom:document.querySelector("#bottom").getBoundingClientRect().y,gutter:document.documentElement.style.scrollbarGutter})'),beforeFullGeometry,'Full-page capture must preserve layout and scroll');
  const fullPng=await readFile(fullShot.images[0].path);assert.ok(fullPng.readUInt32BE(20)>fullShot.pages[0].viewport.height);
  const fullDocument=JSON.parse(await readFile(fullShot.documentPath,'utf8'));const bottom=fullDocument.pages[0].items.find(i=>i.name==='整页底部按钮').documentRect;
  await page.evaluate(`document.addEventListener('click',e=>window.lastClick={x:e.clientX,y:e.clientY,target:e.target.outerHTML.slice(0,300)},true)`);
  assert.equal((await act({action:'click_at',x:bottom.x+bottom.width/2,y:bottom.y+bottom.height/2},fullShot)).status,'executed');
  assert.equal(await page.evaluate('window.bottomClicked'),true,JSON.stringify(await page.evaluate('({scroll:scrollY,rect:document.querySelector("#bottom").getBoundingClientRect().toJSON(),click:window.lastClick})')));
  fullPageEvidence={items:document.pages[0].items.length,documentSize:fullShot.documentSize,images:fullShot.images};
  await page.evaluate('document.querySelector("#whole-page").remove();scrollTo(0,0)');
  // Bound huge screenshots without silently dropping the tail; it remains available through nextTile.
  await page.evaluate(`document.body.insertAdjacentHTML('beforeend','<div id="huge" style="height:18000px">超长页面</div>')`);
  const huge=await ui('screenshot');assert.equal(huge.screenshotComplete,false);assert.ok(huge.nextTile>0);
  const tail=await ui('screenshot',{tileOffset:huge.nextTile});assert.equal(tail.nextTile,null);assert.ok(tail.images[0].y>=16384);
  await page.evaluate('document.querySelector("#huge").remove();scrollTo(0,0)');
  const firstTab=(await ui('status')).activeTab;
  const before=await page.evaluate('({value:document.querySelector("input").value,scroll:scrollY})');
  await page.evaluate(`const a=document.createElement('a');a.href='http://127.0.0.1:${port}/popup';a.target='_blank';a.textContent='新窗口';document.body.prepend(a);a.click()`);
  await until(async()=> (await ui('status')).tabs.length===2,'popup tab');
  const popupState=await ui('status');assert.notEqual(popupState.activeTab,firstTab);
  await until(async()=> (await ui('status')).url.includes('/popup'),'popup navigation');
  await ui('select_tab',{tabId:firstTab});
  assert.deepEqual(await page.evaluate('({value:document.querySelector("input").value,scroll:scrollY})'),before);
  const other=await invoke('create_thread',{cwd:root,agentKind:'lyra',model:'',mode:'build',ephemeral:false});
  const bounds=await main.evaluate('(()=>{const r=document.querySelector(".workspace-browser-surface").getBoundingClientRect();return {x:r.x,y:r.y,width:r.width,height:r.height}})()');
  await invoke('native_browser_ui',{threadId:other.id,operation:'mount',args:{}});await invoke('report_activity',{threadId:other.id});
  await ui('mount');await invoke('report_activity',{threadId:thread.id});await ui('layout',{visible:true,tabId:firstTab,...bounds});
  assert.equal((await ui('status')).tabs.length,2);
  assert.deepEqual(await page.evaluate('({value:document.querySelector("input").value,scroll:scrollY})'),before);
  await ui('close_tab',{tabId:popupState.activeTab});
  await page.evaluate(`window.open('about:blank','delayedPopup');setTimeout(()=>{const w=window.open('','delayedPopup');w.location='http://127.0.0.1:${port}/delayed'},100)`);
  await until(async()=> (await ui('status')).tabs.some(t=>t.url.includes('/delayed')),'delayed popup');
  const delayed=await attach(await until(async()=> (await targets()).find(t=>t.url.includes('/delayed')),'delayed target'));
  assert.equal(await delayed.evaluate('!!window.opener'),true);
  await ui('close_tab',{tabId:(await ui('status')).activeTab});assert.equal((await ui('status')).activeTab,firstTab);
  }
  // Exercise the real Chrome transport + shared engine with an emulated extension.
  // This is not a real Chrome extension end-to-end test.
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
        if(command.operation==='tabs')result={tabs:[{tag,allowed:true,controllable:true,title:'Fixture'}]};
        else {assert.equal(command.args.tabTag,tag);assert.equal(command.operation,'cdp');if(command.args.method==='Page.captureScreenshot')chromeCaptures.push(command.args.params);result=await page.call(command.args.method,command.args.params,command.args.sessionId);}
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
    const changed=await chromeUi('act',{tabTag:tag,snapshotId:chromeObs.snapshotId,action:{action:'fill',...find(chromeObs,'订单号'),text:'Chrome桥接验证'}});
    assert.equal(changed.status,'executed');assert.ok(changed.snapshotId);
    assert.equal(await page.evaluate('document.querySelector("input").value'),'Chrome桥接验证');
    const screenshot=await chromeUi('screenshot',{tabTag:tag,fullPage:false});
    assert.ok(screenshot.images.length);assert.equal(screenshot.browser,'chrome');
    // Exercise the compiled pixel preflight, image mapping and post-hover checks.
    await page.evaluate(`document.body.insertAdjacentHTML('beforeend','<style>#hover-option{position:fixed;left:300px;top:200px;width:200px;height:40px;background:white;z-index:99999}#hover-option:hover{background:rgb(40,150,240)}</style><div id="hover-option" onclick="window.optionClicked=(window.optionClicked||0)+1">Leave-Pay</div>')`);
    for(const deviceScaleFactor of [1,1.25,2]) {
    await page.call('Emulation.setDeviceMetricsOverride',{width:900,height:700,deviceScaleFactor,mobile:false});
    for(const options of [{maxEdge:320},{maxEdge:0,region:{x:280.4,y:180.4,width:240.2,height:80.2}}]) {
      await page.call('Input.dispatchMouseEvent',{type:'mouseMoved',x:10,y:10});
      const geometry=await page.evaluate('document.querySelector("#hover-option").getBoundingClientRect().toJSON()');
      const shot=await chromeUi('screenshot',{tabTag:tag,fullPage:false,...options});
      assert.deepEqual(await page.evaluate('document.querySelector("#hover-option").getBoundingClientRect().toJSON()'),geometry);
      const image=shot.images[0], count=chromeCaptures.length;
      const clicked=await chromeUi('act',{tabTag:tag,snapshotId:shot.snapshotId,imageId:image.imageId,feedback:'screenshot',action:{action:'click_at',x:(340-image.x)*image.pixelWidth/image.width,y:(220-image.y)*image.pixelHeight/image.height}});
      assert.equal(clicked.status,'executed',JSON.stringify(clicked));
      assert.equal(chromeCaptures.length-count,2,'one preflight and one feedback; DOM hover does not need a second screenshot');
    }
    }
    await page.call('Emulation.clearDeviceMetricsOverride');
    assert.equal(await page.evaluate('window.optionClicked'),6);
    assert.ok(chromeCaptures.every(params=>!params.clip && params.fromSurface===true && params.captureBeyondViewport===false),'viewport captures must not override the browser render scale/clip');
    await page.call('Input.dispatchMouseEvent',{type:'mouseMoved',x:10,y:10});
    const stale=await chromeUi('screenshot',{tabTag:tag,fullPage:false});
    await page.evaluate('document.querySelector("#hover-option").style.background="black"');
    const refused=await chromeUi('act',{tabTag:tag,snapshotId:stale.snapshotId,action:{action:'click_at',x:340,y:220}});
    assert.equal(refused.status,'not_executed');assert.match(refused.reason,/画面已变化/);
    assert.equal(await page.evaluate('window.optionClicked'),6,'pre-existing visual changes still block input');
    await page.evaluate('document.querySelector("#hover-option").remove()');
    if(pollingError)throw pollingError;
    assert.equal(await main.evaluate('!!document.querySelector("[aria-label=浏览器来源]")'),false);
    assert.equal(await main.evaluate('!!document.querySelector(".workspace-browser-surface")'),true);
  }finally{abort.abort();await polling.catch(()=>{});}
  if(coordinateOnly) {
    await writeFile(join(output,'coordinate-report.json'),JSON.stringify({passed:true,modelCalls:0,coordinateHover:true,localCrop:true,stalePixels:true,chromeCaptures,profile},null,2));
    console.log('PASS compiled coordinate hover, scaled/cropped screenshots, stale pixel rejection and clip-free capture via emulated Chrome transport');
  } else {
  const pageShot=await page.call('Page.captureScreenshot',{format:'png'});await writeFile(join(output,'page.png'),Buffer.from(pageShot.data,'base64'));
  const compactShot=await main.call('Page.captureScreenshot',{format:'png'});await writeFile(join(output,'browser-ui.png'),Buffer.from(compactShot.data,'base64'));
  await main.evaluate(`Array.from(document.querySelectorAll('.workspace-modes button')).find(b=>b.textContent==='文件').click()`);
  await until(async()=> !(await ui('status')).visible,'hide on tab switch');
  await main.evaluate(`Array.from(document.querySelectorAll('.workspace-modes button')).find(b=>b.textContent==='浏览器').click()`);
  await until(async()=> (await ui('status')).visible,'restore tab');
  await main.evaluate('document.querySelector(".settings-btn").click()');
  await until(async()=> !(await ui('status')).visible,'hide for settings');
  assert.equal(await main.evaluate('document.body.textContent.includes("辅助控制模型")'),false);
  await writeFile(join(output,'report.json'),JSON.stringify({passed:true,modelCalls:0,actionWithFeedbackMs:timings,optimization:{domRefAfter31Seconds:true,coordinateAfter31Seconds:true,chainedFeedback:true,replacedElementRejected:true,menuStateAndHref:true,screenshotFeedback:true,summaryBytes},fullPageEvidence,screenshot:observation.path,profile},null,2));
  console.log('PASS native browser full-document/full-page/tiles/document-coordinates/DOM/native-input/popups/session-state/stop + Chrome emulated transport/engine; no model calls: '+output);
  }
} finally {
  for (const socket of sockets) socket.close();
  if (child.exitCode === null && child.pid) spawnSync('taskkill',['/PID',String(child.pid),'/T','/F'],{windowsHide:true,stdio:'ignore'});
  server.closeAllConnections(); await new Promise(r=>server.close(r));
  await writeFile(join(output,'stderr.txt'),stderr);
}
