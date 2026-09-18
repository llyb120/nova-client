// Windows acceptance: real Nova Rust tool + real Chrome/CDP. The extension's
// authenticated poll/reply transport is simulated, not DOM/input or screenshots.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { spawn, spawnSync } from 'node:child_process';
import { access, mkdir, mkdtemp, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { resolve, join } from 'node:path';
import { setTimeout as sleep } from 'node:timers/promises';
const output=resolve('src-tauri/target/chrome-precision-smoke');await mkdir(output,{recursive:true});
const profile=await mkdtemp(join(tmpdir(),'nova-chrome-precision-'));
const appPort=Number(process.env.TEST_CDP_PORT || 9222),browserPort=Number(process.env.TEST_CHROME_PORT || 9333);
const sockets=[],processes=[],checks=[];let appLog='',polling,abort;
const server=createServer((req,res)=>{res.setHeader('Content-Type','text/html; charset=utf-8');res.end(req.url==='/frame'?'<button id="frame" onclick="this.textContent=\'框架成功\'">框架按钮</button>':`<!doctype html><meta charset="utf-8"><style>body{margin:12px;font:16px sans-serif}section{margin:8px;padding:10px;border:1px solid}button,input{padding:8px}canvas{display:block;width:320px;height:160px}iframe{width:250px;height:90px;border:4px solid;transform-origin:0 0;transform:scale(1.25)}</style><section>客户筛选<button id="wrong" onclick="window.wrong=true">查询</button></section><section>订单筛选<label>订单号<input id="order" oninput="window.inputTrusted=event.isTrusted"></label><button id="run" onclick="document.querySelector('output').textContent=document.querySelector('#order').value+' 已发货'">查询</button><output></output></section><input id="trap" aria-label="其他字段" value="preserve"><div role="row" id="row"><span>订单 A</span><button onclick="window.deleted=true">删除</button></div><fieldset disabled><button>等待启用</button></fieldset><canvas id="paint" aria-label="精确画板" width="1280" height="640"></canvas><iframe src="http://localhost:${server.address().port}/frame"></iframe>`);});
await new Promise(r=>server.listen(0,'127.0.0.1',r));
async function until(fn,label,ms=45000){const end=Date.now()+ms;let error;while(Date.now()<end){try{const value=await fn();if(value)return value;}catch(e){error=e;}await sleep(100);}throw Error(`${label}: ${error || 'timeout'}`);}
async function attach(port,predicate=()=>true){const target=await until(async()=>{const list=await (await fetch(`http://127.0.0.1:${port}/json/list`,{signal:AbortSignal.timeout(1000)})).json();return list.find(t=>t.type==='page'&&predicate(t));},'CDP target '+port);const socket=new WebSocket(target.webSocketDebuggerUrl);sockets.push(socket);await new Promise((r,j)=>{socket.addEventListener('open',r,{once:true});socket.addEventListener('error',j,{once:true});});const pending=new Map();let sequence=0;socket.addEventListener('message',({data})=>{const m=JSON.parse(data),job=pending.get(m.id);if(!job)return;clearTimeout(job.timer);pending.delete(m.id);m.error?job.reject(Error(JSON.stringify(m.error))):job.resolve(m.result);});const call=(method,params={},sessionId)=>new Promise((resolve,reject)=>{const id=++sequence,timer=setTimeout(()=>{pending.delete(id);reject(Error(method+' timed out'));},30000);pending.set(id,{resolve,reject,timer});socket.send(JSON.stringify({id,method,params,...(sessionId?{sessionId}:{})}));});const evaluate=async expression=>{const r=await call('Runtime.evaluate',{expression,returnByValue:true,awaitPromise:true});if(r.exceptionDetails)throw Error(JSON.stringify(r.exceptionDetails));return r.result.value;};return {call,evaluate};}
const check=(name)=>{checks.push(name);console.log('PASS',name);};
try{
  let browserExe;
  for(const file of [process.env.TEST_CHROME,'C:/Program Files/Google/Chrome/Application/chrome.exe','C:/Program Files (x86)/Google/Chrome/Application/chrome.exe','C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe'].filter(Boolean)){try{await access(file);browserExe=file;break;}catch{}}
  assert.ok(browserExe,'No installed Chromium browser');
  const browser=spawn(browserExe,['--headless=new',`--remote-debugging-port=${browserPort}`,`--user-data-dir=${join(profile,'chrome')}`,'--no-first-run','--no-default-browser-check','about:blank'],{stdio:'ignore'});processes.push(browser);
  const nova=spawn(resolve(process.argv[2]||'src-tauri/target/debug/nova.exe'),[],{env:{...process.env,NOVA_CHROME_PORT:'0',NOVA_DATA_DIR:join(profile,'nova'),WEBVIEW2_USER_DATA_FOLDER:join(profile,'webview')},stdio:['ignore','ignore','pipe']});processes.push(nova);nova.stderr.on('data',d=>{appLog+=d;});
  const main=await attach(appPort,t=>!t.url.startsWith('devtools:'));
  await until(()=>main.evaluate('!!window.__TAURI_INTERNALS__ && !!document.querySelector(".app")'),'Nova UI');
  await main.call('Emulation.setFocusEmulationEnabled',{enabled:true});
  const native=(command,args={})=>main.evaluate(`window.__TAURI_INTERNALS__.invoke(${JSON.stringify(command)},${JSON.stringify(args)})`);
  // Exercise the UI entry point under a real local thread, without starting an agent.
  const thread=await native('create_thread',{cwd:process.cwd(),agentKind:'lyra',model:'',mode:'build',ephemeral:false});
  await until(()=>main.evaluate('!!document.querySelector(".thread-item")'),'fixture thread');
  await main.evaluate('document.querySelector(".thread-item").click()');
  await native('report_activity',{threadId:thread.id});
  const invoke=(operation,args={})=>main.evaluate(`window.__TAURI_INTERNALS__.invoke('chrome_browser_ui',${JSON.stringify({operation,args})})`);
  const page=await attach(browserPort);const version=await page.call('Browser.getVersion');
  await page.call('Page.navigate',{url:`http://127.0.0.1:${server.address().port}/fixture`});
  await until(()=>page.evaluate('!!document.querySelector("#paint")'),'fixture');
  await page.evaluate(`const c=document.querySelector('#paint'),g=c.getContext('2d');g.fillStyle='white';g.fillRect(0,0,1280,640);g.fillStyle='red';g.fillRect(400,160,480,320);window.canvasEvents=[];for(const type of ['click','dblclick','pointermove','wheel'])c.addEventListener(type,e=>{canvasEvents.push({type,trusted:e.isTrusted,buttons:e.buttons,deltaX:e.deltaX});if(type==='wheel')e.preventDefault();},{passive:false});`);
  const connection=await invoke('connect'),origin=`chrome-extension://${connection.extensionId}`;
  const pairing=await fetch(connection.origin+'/pair',{method:'POST',headers:{Origin:origin}});assert.equal(pairing.status,200);
  const config=await pairing.json(),tag='C1-precisiontest';abort=new AbortController();
  const post=async(route,body)=>{const r=await fetch(connection.origin+route,{method:'POST',headers:{'Content-Type':'application/json',Origin:origin,Authorization:`Bearer ${config.token}`},body:JSON.stringify({clientId:'12345678-1234-4234-8234-123456789abc',...body}),signal:abort.signal});assert.equal(r.status,200);return r.json();};
  polling=(async()=>{while(!abort.signal.aborted){const {command}=await post('/poll',{});if(!command)continue;let reply;try{let result;if(command.operation==='tabs')result={tabs:[{tag,allowed:true,controllable:true,title:'Precision fixture'}]};else{assert.equal(command.operation,'cdp');assert.equal(command.args.tabTag,tag);result=await page.call(command.args.method,command.args.params,command.args.sessionId);}reply={id:command.id,result};}catch(e){reply={id:command.id,error:String(e)};}await post('/reply',reply);}})();
  let pollError;polling.catch(e=>{if(!abort.signal.aborted)pollError=e;});
  await until(async()=> (await invoke('status')).connected,'Chrome bridge');
  const ui=(op,args={})=>invoke(op,{tabTag:tag,...args});
  const inspect=(args={})=>ui('inspect',{includeVisual:false,...args});
  const find=(o,name,region)=>{for(const p of o.pages){const item=p.items.find(i=>i.name===name && i.role!=='scroll-container'&&(!region||i.region.includes(region)));if(item)return {frame:p.frame,ref:item.ref};}throw Error('missing '+name+':'+JSON.stringify(o));};
  const act=(o,action,extra={})=>ui('act',{snapshotId:o.snapshotId,action,includeVisual:false,...extra});
  const expect=(o,status)=>assert.equal(o.status,status,JSON.stringify({status:o.status,reason:o.reason,releaseErrors:o.releaseErrors}));
  let o=await inspect();o=await act(o,{action:'fill',...find(o,'订单号'),text:'订单-123'});expect(o,'executed');assert.equal(await page.evaluate('document.querySelector("#order").value'),'订单-123');assert.equal(await page.evaluate('window.inputTrusted'),true);
  o=await act(o,{action:'click',...find(o,'查询','订单筛选')});expect(o,'executed');assert.equal(await page.evaluate('document.querySelector("output").textContent'),'订单-123 已发货');assert.notEqual(await page.evaluate('window.wrong'),true);check('trusted fill and same-name button disambiguation');
  o=await inspect();const deletion=find(o,'删除');await page.evaluate('document.querySelector("#row span").textContent="订单 B"');o=await act(o,{action:'click',...deletion});expect(o,'not_executed');assert.notEqual(await page.evaluate('window.deleted'),true);check('recycled data-row reference rejected before click');
  await page.call('Input.dispatchMouseEvent',{type:'mouseMoved',x:0,y:0});o=await inspect();const hover=find(o,'查询','订单筛选');await page.evaluate('document.querySelector("#run").onmouseenter=function(){this.style.transform="translateX(120px)"}');o=await act(o,{action:'click',...hover});expect(o,'not_executed');await page.evaluate('document.querySelector("#run").onmouseenter=null;document.querySelector("#run").style.transform=""');check('hover-induced movement rejected');
  await page.evaluate('document.querySelector("#order").addEventListener("focus",()=>document.querySelector("#trap").focus(),{once:true})');o=await inspect();o=await act(o,{action:'fill',...find(o,'订单号'),text:'must not reach trap'});expect(o,'needs_review');assert.equal(await page.evaluate('document.querySelector("#trap").value'),'preserve');check('focus theft classified needs_review without typing into wrong input');
  o=await inspect();const wait=find(o,'等待启用');await page.evaluate('setTimeout(()=>document.querySelector("fieldset").disabled=false,100)');o=await act(o,{action:'wait_for',...wait,state:'enabled',ms:1000});expect(o,'executed');check('condition wait');
  o=await until(async()=>{const v=await inspect({query:'框架按钮'});return v.pages.some(p=>p.items.some(i=>i.name==='框架按钮'))&&v;},'iframe');o=await act(o,{action:'click',...find(o,'框架按钮')});expect(o,'executed');assert.ok(JSON.stringify(o).includes('框架成功'));check('scaled cross-origin iframe native click');
  await page.evaluate('scrollTo(0,0)');o=await ui('inspect',{query:'only painted pixels'});assert.equal(o.visualRequired,true);assert.ok(o.images[0].imageId);check('Canvas auto screenshot survives unmatched DOM query');
  const rect=await page.evaluate('document.querySelector("#paint").getBoundingClientRect().toJSON()');
  const coords=o=>{const i=o.images[0];return {imageId:i.imageId,x:(rect.x+rect.width/2-i.x)*i.pixelWidth/i.width,y:(rect.y+rect.height/2-i.y)*i.pixelHeight/i.height};};
  const canvasAct=(o,action)=>act(o,action,{includeVisual:true,feedback:'screenshot'});
  o=await canvasAct(o,{action:'click_at',...coords(o)});expect(o,'executed');assert.equal(await page.evaluate('canvasEvents.filter(e=>e.type==="click"&&e.trusted).length'),1);check('Canvas image pixels mapped to trusted click');
  await page.evaluate('const g=document.querySelector("#paint").getContext("2d");g.fillStyle="blue";g.fillRect(400,160,480,320)');o=await canvasAct(o,{action:'click_at',...coords(o)});expect(o,'not_executed');assert.match(o.reason,/画面已变化/);assert.equal(await page.evaluate('canvasEvents.filter(e=>e.type==="click").length'),1);check('paint-only change rejected without additional click');
  o=await ui('screenshot',{fullPage:false,region:{x:rect.x,y:rect.y,width:rect.width,height:rect.height}});const image=o.images[0];o=await canvasAct(o,{action:'double_click_at',imageId:image.imageId,x:image.pixelWidth/2,y:image.pixelHeight/2});expect(o,'executed');assert.equal(await page.evaluate('canvasEvents.filter(e=>e.type==="dblclick"&&e.trusted).length'),1);check('cropped image native double click');
  const start=coords(o),map=o.images[0];o=await canvasAct(o,{action:'drag',...start,to_x:start.x+30*map.pixelWidth/map.width,to_y:start.y});expect(o,'executed');assert.ok(await page.evaluate('canvasEvents.filter(e=>e.type==="pointermove"&&e.buttons===1).length>=8'));check('continuous native drag path');
  o=await canvasAct(o,{action:'scroll_at',...coords(o),delta:0,delta_x:40});expect(o,'executed');await until(()=>page.evaluate('canvasEvents.some(e=>e.type==="wheel"&&e.deltaX===40&&e.trusted)'),'horizontal wheel');check('horizontal Canvas wheel');
  if(pollError)throw pollError;
  const shot=await page.call('Page.captureScreenshot',{format:'png'});await writeFile(join(output,'chrome.png'),Buffer.from(shot.data,'base64'));
  await writeFile(join(output,'report.json'),JSON.stringify({passed:true,browser:version,checks,transport:'authenticated extension poll/reply simulated; actual Rust engine and real browser input'},null,2));
}catch(e){await writeFile(join(output,'failure.txt'),String(e.stack||e));throw e;}finally{abort?.abort();await polling?.catch(()=>{});for(const s of sockets)s.close();for(const p of processes)if(p.pid)spawnSync('taskkill.exe',['/PID',String(p.pid),'/T','/F'],{windowsHide:true,stdio:'ignore'});server.closeAllConnections();await new Promise(r=>server.close(r));await writeFile(join(output,'nova.log'),appLog);}
