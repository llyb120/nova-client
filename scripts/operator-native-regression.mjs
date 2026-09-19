// Deterministic native control regression, NOT a model benchmark.
// Browser setup/geometry/oracle use Playwright; every tested input uses production Nova native tools.
import {createServer} from 'node:http';
import {spawn} from 'node:child_process';
import {createInterface} from 'node:readline';
import {mkdtemp,mkdir,readFile,writeFile,rm,cp} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {resolve,join} from 'node:path';
import {createRequire} from 'node:module';
import assert from 'node:assert/strict';
const require=createRequire(import.meta.url),pause=ms=>new Promise(r=>setTimeout(r,ms));
const out=resolve('validation/native-regression');await mkdir(out,{recursive:true});
const root=await mkdtemp(join(tmpdir(),'nova-native-regression-')),data=join(root,'data');await mkdir(data,{recursive:true});
const report={kind:'deterministic-production-native-controls',modelCalls:0,scope:'Not a model A/B: real Chromium/CDP and XCap/Enigo actions; fixture geometry from test driver.',cases:[],calls:[]};
let proc,browser,server,serial=0;const waiters=new Map();
try{
 const html=await readFile('bench/operator-gui-harness/test-app.html');
 server=createServer((req,res)=>{res.writeHead(200,{'Content-Type':'text/html'});res.end(html)});await new Promise(r=>server.listen(0,'127.0.0.1',r));const origin=`http://127.0.0.1:${server.address().port}`;
 proc=spawn(resolve(process.env.OPERATOR_GUI_EXE),[],{env:{...process.env,OPERATOR_TEST_WORKSPACE:root,OPERATOR_TEST_DATA:data,OPERATOR_TEST_MODEL:'no-model-deterministic-regression',OPERATOR_TEST_CALLBACK:origin+'/unused',OPERATOR_TEST_CALLBACK_TOKEN:'unused-fixture-token'},stdio:['pipe','pipe','pipe']});
 let readyResolve;const ready=new Promise(r=>readyResolve=r);let stderr='';proc.stderr.on('data',b=>stderr=(stderr+b).slice(-20000));
 createInterface({input:proc.stdout}).on('line',line=>{let m;try{m=JSON.parse(line)}catch{return}if(m.ready){readyResolve();return}const w=waiters.get(m.id);if(w){clearTimeout(w.t);waiters.delete(m.id);m.error?w.reject(Error(m.error)):w.resolve(m.result)}});
 proc.on('exit',code=>{for(const w of waiters.values()){clearTimeout(w.t);w.reject(Error(`Native exited ${code}`))}waiters.clear()});
 const request=(method,args={})=>new Promise((resolve,reject)=>{const id=++serial,t=setTimeout(()=>{waiters.delete(id);reject(Error('Native timeout'))},20000);waiters.set(id,{resolve,reject,t});proc.stdin.write(JSON.stringify({id,method,...args})+'\n')});
 const native=async(channel,args)=>{const start=performance.now();try{const value=await request('native',{channel,args});report.calls.push({channel,args,result:value,elapsedMs:performance.now()-start});return value}catch(e){report.calls.push({channel,args,error:e.message});throw e}};
 await Promise.race([ready,pause(15000).then(()=>{throw Error('Native startup timeout '+stderr)})]);
 const {chromium}=require(resolve('bench/operator-gui-deps/node_modules/playwright'));const extension=resolve('extensions/nova-chrome');
 browser=await chromium.launchPersistentContext(join(root,'browser'),{channel:'chromium',headless:false,viewport:{width:1000,height:720},args:['--no-sandbox','--window-position=0,0','--window-size=1000,850',`--disable-extensions-except=${extension}`,`--load-extension=${extension}`]});
 const page=browser.pages()[0]??await browser.newPage();await page.goto(origin+'/app?case=form');await page.bringToFront();
 let connected=false;for(let n=0;n<30;n++){if((await native('chrome',{operation:'status'})).connected){connected=true;break}await pause(500)}assert.ok(connected,'extension paired');
 const tab=(await native('chrome',{operation:'tabs'})).tabs.find(t=>t.url===page.url()).tag;
 const reset=async c=>{await page.goto(origin+'/app?case='+c);await page.bringToFront();await pause(500)};
 const test=async(name,fn)=>{const r={name};try{await fn(r);r.passed=true}catch(e){r.passed=false;r.error=e.message}report.cases.push(r);console.log(JSON.stringify(r));await writeFile(join(out,'report.json'),JSON.stringify(report,null,2))};
 const screenPoint=async(selector,rx=.5,ry=.5)=>page.evaluate(({selector,rx,ry})=>{const r=document.querySelector(selector).getBoundingClientRect();return{x:screenX+(outerWidth-innerWidth)/2+r.x+r.width*rx,y:screenY+(outerHeight-innerHeight)-4+r.y+r.height*ry}},{selector,rx,ry});
 const shot=()=>native('jianlai',{operation:'screenshot',maxEdge:1400});
 const imagePoint=(s,p)=>{const im=s.images.find(i=>i.desktopBounds&&p.x>=i.desktopBounds.x&&p.y>=i.desktopBounds.y&&p.x<i.desktopBounds.x+i.desktopBounds.width&&p.y<i.desktopBounds.y+i.desktopBounds.height);assert.ok(im,'point on screenshot');return{imageId:im.imageId,x:Math.round((p.x-im.desktopBounds.x)*(im.pixelWidth??im.width)/im.desktopBounds.width),y:Math.round((p.y-im.desktopBounds.y)*(im.pixelHeight??im.height)/im.desktopBounds.height)}};
 await test('chrome real input and offscreen fill',async r=>{await reset('form');for(const [name,text]of [['Title','September review'],['Notes','Checked by Nova']]){const s=await native('chrome',{operation:'inspect',tabTag:tab});const item=s.pages.flatMap(p=>p.items.map(i=>({...i,frame:p.frame}))).find(i=>i.name===name);assert.ok(item);const a=await native('chrome',{operation:'act',tabTag:tab,snapshotId:s.snapshotId,action:{action:'fill',frame:item.frame,ref:item.ref,text}});assert.equal(a.status,'executed')}
  const s=await page.evaluate(()=>window.__oracle());r.state={title:s.title,notes:s.notes};assert.equal(s.title,'September review');assert.equal(s.notes,'Checked by Nova');assert.ok(s.events.some(e=>e.trusted&&e.type==='input'));});
 for(const key of ['Ctrl+A','Ctrl+a'])await test('jianlai real shortcut '+key,async r=>{await reset('form');const p=await screenPoint('#title'),s=await shot(),v=imagePoint(s,p);const a=await native('jianlai',{operation:'act',snapshotId:s.snapshotId,imageId:v.imageId,actions:[{action:'click',x:v.x,y:v.y},{action:'press',key},{action:'type',text:'Replacement'}]});r.nativeStatus=a.status;const state=await page.evaluate(()=>window.__oracle());r.title=state.title;r.trustedEvents=state.events.filter(e=>e.trusted).length;assert.equal(a.status,'executed');assert.equal(state.title,'Replacement');});
 await test('jianlai stale snapshot rejected before input',async r=>{await reset('form');const p=await screenPoint('#title'),old=await shot(),v=imagePoint(old,p);await shot();const a=await native('jianlai',{operation:'act',snapshotId:old.snapshotId,imageId:v.imageId,actions:[{action:'click',x:v.x,y:v.y},{action:'type',text:'WRONG'}]});r.nativeStatus=a.status;r.completedActions=a.completedActions;assert.equal(a.status,'not_executed');assert.equal(a.completedActions,0);assert.equal((await page.evaluate(()=>window.__oracle())).title,'Old draft')});
 await test('jianlai real canvas drag',async r=>{await reset('canvas');const a=await screenPoint('#board',165/850,142/420),b=await screenPoint('#board',682/850,299/420),s=await shot(),p=imagePoint(s,a),q=imagePoint(s,b);assert.equal(p.imageId,q.imageId);const result=await native('jianlai',{operation:'act',snapshotId:s.snapshotId,imageId:p.imageId,actions:[{action:'drag',x:p.x,y:p.y,toX:q.x,toY:q.y}]});r.nativeStatus=result.status;r.canvas=(await page.evaluate(()=>window.__oracle())).canvas;assert.equal(result.status,'executed');assert.equal(r.canvas.aligned,true);await page.screenshot({path:join(out,'canvas-after.png')});});
 report.status='completed';report.stderr=stderr;
}catch(e){report.status='blocked';report.error=e.message;process.exitCode=1}
finally{report.nativeActionCalls=report.calls.filter(c=>c.args.operation==='act').length;report.passed=report.cases.filter(c=>c.passed).length;await writeFile(join(out,'report.json'),JSON.stringify(report,null,2));await cp(join(data,'desktop-shots'),join(out,'desktop-shots'),{recursive:true}).catch(()=>{});await browser?.close().catch(()=>{});if(proc){proc.stdin.end(JSON.stringify({id:++serial,method:'close'})+'\n');await pause(500);proc.kill()}server?.closeAllConnections();server?.close();await rm(root,{recursive:true,force:true})}
