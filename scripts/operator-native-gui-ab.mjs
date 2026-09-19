// Actual headed Chromium + real production Nova extension/CDP and Jianlai Enigo/XCap.
// Playwright is ONLY setup and the post-run oracle. It never executes model actions.
import {createServer} from 'node:http';
import {spawn} from 'node:child_process';
import {createInterface} from 'node:readline';
import {readFile,writeFile,mkdir,mkdtemp,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {createHash,randomBytes} from 'node:crypto';
import {createRequire} from 'node:module';
import {performance} from 'node:perf_hooks';
import {pathToFileURL} from 'node:url';
const require=createRequire(import.meta.url);
const MODEL='deepseek/deepseek-v4.1-flash';
const API='https://api.commandcode.ai/provider/v1';
const pause=ms=>new Promise(r=>setTimeout(r,ms));
const hash=b=>createHash('sha256').update(b).digest('hex');
const redact=(s,key)=>String(s).split(key||'__no_key__').join('[REDACTED]').replace(/user_[A-Za-z0-9_-]{16,}/g,'[REDACTED]');
const pendingOrders=[{id:'A101',amount:'12.50'},{id:'A103',amount:'25.00'},{id:'A105',amount:'9.75'},{id:'A106',amount:'18.25'},{id:'A108',amount:'31.00'},{id:'A109',amount:'16.50'}];
const cases=[
 {id:'form',goal:'In the visible Local draft editor, change Title to September review, set Priority to High, scroll down and replace Notes with Checked by Nova. These are reversible local draft edits; do not send anything. Verify all three values before finishing.',acceptance:['Title is September review','Priority is High','Notes is Checked by Nova and local save is visible'],verify:(s,r)=>s.title==='September review'&&s.priority==='High'&&s.notes==='Checked by Nova'},
 {id:'orders',goal:'On the Orders page, apply the Pending filter, read ALL result pages and return result.orders as an array of {id,amount}, with amount as the displayed two-decimal string. Preserve earlier-page records in checkpoint. Do not modify any order.',acceptance:['Pending filter applied','Every filtered page read, including End of results','All pending order IDs and amounts returned'],verify:(s,r)=>s.filter==='pending'&&[1,2].every(p=>s.pagesVisited.some(v=>v.filter==='pending'&&v.page===p))&&JSON.stringify((r?.orders||[]).map(o=>({id:o.id,amount:o.amount})).sort((a,b)=>a.id.localeCompare(b.id)))===JSON.stringify(pendingOrders)},
 {id:'canvas',goal:'On the Canvas board, drag the blue NOVA card fully inside the dashed DROP ZONE. Verify that the page displays Aligned — task complete. Use screenshots for the card and target positions; do not guess positions from old images. This only changes a local practice canvas.',acceptance:['Blue NOVA card moved fully inside the dashed box','Aligned — task complete is visible'],verify:(s,r)=>s.canvas.dragged===true&&s.canvas.aligned===true}
];
export async function runGui({apiKey,outDir='validation/gui',setupOnly=false}){
 if(!setupOnly&&(!apiKey||!/^user_[A-Za-z0-9_-]{16,300}$/.test(apiKey)))throw Error('Missing valid credential');
 await mkdir(outDir,{recursive:true});
 const root=await mkdtemp(join(tmpdir(),'nova-real-gui-'));const data=join(root,'data');await mkdir(data,{recursive:true});
 const token=randomBytes(24).toString('hex');
 const report={kind:'real-native-gui-model-ab',model:MODEL,sourceCommit:process.env.GITHUB_SHA??null,productionBaseline:'517cb2d43c54acb89d38c33b9d89095362394a8b',startedAt:new Date().toISOString(),environment:{platform:process.platform,display:process.env.DISPLAY},scope:'Actual production Operator runtime, native Chrome extension/CDP and Jianlai OS input/capture. Isolated test application and Tauri shell. Not CodeBuddy CLI or a user desktop.',arms:{A:'Same production runtime, plus cumulative real observations/images in model input',B:'Unchanged production runtime/projector, current observation only'},modelCallsAttempted:0,modelCallsSucceeded:0,actualUsage:{prompt_tokens:0,completion_tokens:0},runs:[],nativeEvents:[],decisions:[],setupOnly,status:'running'};
 const save=()=>writeFile(join(outDir,'report.json'),redact(JSON.stringify(report,null,2),apiKey),{mode:0o600});
 let active,browser,proc,server,serial=0,stopped=false;const waiters=new Map();
 const appHtml=await readFile('bench/operator-gui-harness/test-app.html');
 async function model(input){
   if(!active||input.run!==active.id)throw Error('Wrong active run');
   if(setupOnly)throw Error('Setup-only run does not call models');
   if(report.modelCallsAttempted>=180||report.actualUsage.prompt_tokens+report.actualUsage.completion_tokens>=2000000)throw Error('Global model budget reached');
   if(active.modelRequests>=20)throw Error('Per-task decision limit reached');
   const context=structuredClone(input.context);const ob=context.currentObservation;
   if(ob&&!active.history.some(o=>o.ob.evidenceId===ob.evidenceId))active.history.push({ob:structuredClone(ob),images:input.images});
   let imageGroups=ob?[{ob,images:input.images}]:[];
   if(active.arm==='A'){
     context.observationHistory=active.history.map(v=>v.ob);
     imageGroups=active.history;
   }
   const content=[{type:'text',text:JSON.stringify(context)}];
   for(const group of imageGroups)for(let i=0;i<group.images.length;i++){
     const image=group.images[i];
     content.push({type:'text',text:`Screenshot evidence ${group.ob.evidenceId}; imageId ${group.ob.data.images?.[i]?.imageId??'unknown'}; ${group.ob.evidenceId===ob?.evidenceId?'CURRENT':'HISTORICAL — never use its coordinates for current input'}`});
     content.push({type:'image_url',image_url:{url:`data:${image.mimeType};base64,${image.data}`}});
   }
   const body={model:MODEL,messages:[{role:'system',content:input.system},{role:'user',content}],temperature:0,reasoning_effort:'low',max_tokens:1536,stream:false};
   const decision={run:active.id,index:active.modelRequests++,context,images:imageGroups.flatMap(g=>g.images.map((im,i)=>({evidenceId:g.ob.evidenceId,imageId:g.ob.data.images?.[i]?.imageId,sha256:hash(Buffer.from(im.data,'base64')),bytes:Buffer.byteLength(im.data,'base64')}))),inputBytes:Buffer.byteLength(JSON.stringify(body))};
   report.modelCallsAttempted++;const start=performance.now();await save();
   try{
     const response=await fetch(`${API}/chat/completions`,{method:'POST',redirect:'error',headers:{Authorization:`Bearer ${apiKey}`,'Content-Type':'application/json'},body:JSON.stringify(body),signal:AbortSignal.timeout(70000)});
     const result=await response.json();decision.elapsedMs=performance.now()-start;decision.httpStatus=response.status;
     if(!response.ok)throw Error(`Model HTTP ${response.status}: ${redact(result.error?.message??JSON.stringify(result.error),apiKey)}`);
     decision.returnedModel=result.model;decision.usage=result.usage;decision.finishReason=result.choices?.[0]?.finish_reason;decision.text=result.choices?.[0]?.message?.content??'';
     if(result.model!==MODEL)throw Error('Provider returned a different model');
     report.modelCallsSucceeded++;
     report.actualUsage.prompt_tokens+=Number(result.usage?.prompt_tokens)||0;report.actualUsage.completion_tokens+=Number(result.usage?.completion_tokens)||0;
     report.decisions.push(decision);await save();return decision.text;
   }catch(error){decision.error=redact(error.message,apiKey);report.decisions.push(decision);await save();throw error;}
 }
 try{
  server=createServer(async(req,res)=>{
   try{
    if(req.method==='GET'&&req.url.startsWith('/app')){res.writeHead(200,{'Content-Type':'text/html; charset=utf-8','Cache-Control':'no-store'});res.end(appHtml);return;}
    if(req.method!=='POST'||req.url!=='/decide'||req.headers['x-test-token']!==token){res.writeHead(403);res.end();return;}
    let bytes=0,chunks=[];for await(const part of req){bytes+=part.length;if(bytes>40*1024*1024)throw Error('Callback too large');chunks.push(part);}
    const text=await model(JSON.parse(Buffer.concat(chunks)));res.writeHead(200,{'Content-Type':'application/json'});res.end(JSON.stringify({text}));
   }catch(e){res.writeHead(500,{'Content-Type':'application/json'});res.end(JSON.stringify({error:redact(e.message,apiKey)}));}
  });
  await new Promise(r=>server.listen(0,'127.0.0.1',r));const origin=`http://127.0.0.1:${server.address().port}`;
  const bin=process.env.OPERATOR_GUI_EXE||'bench/operator-gui-harness/target/debug/nova-operator-gui-harness';
  proc=spawn(resolve(bin),[],{env:{...process.env,OPERATOR_TEST_WORKSPACE:root,OPERATOR_TEST_DATA:data,OPERATOR_TEST_MODEL:MODEL,OPERATOR_TEST_CALLBACK:`${origin}/decide`,OPERATOR_TEST_CALLBACK_TOKEN:token},stdio:['pipe','pipe','pipe']});
  let readyResolve,readyReject;const ready=new Promise((r,j)=>{readyResolve=r;readyReject=j});
  let stderr='';proc.stderr.on('data',b=>{stderr=(stderr+b).slice(-20000)});
  const lines=createInterface({input:proc.stdout});lines.on('line',line=>{
   let m;try{m=JSON.parse(line)}catch{return}
   if(m.ready){report.nativeReady=m;readyResolve();return;}
   if(m.event==='native'){report.nativeEvents.push(m);return;}
   const waiter=waiters.get(m.id);if(waiter){waiters.delete(m.id);clearTimeout(waiter.timer);m.error?waiter.reject(Error(m.error)):waiter.resolve(m.result);}
  });
  proc.on('error',readyReject);proc.on('exit',code=>{stopped=true;readyReject(Error(`Native process exited ${code}: ${stderr}`));for(const w of waiters.values()){clearTimeout(w.timer);w.reject(Error(`Native process exited ${code}`));}waiters.clear();});
  const request=(method,args={})=>new Promise((resolve,reject)=>{const id=++serial;const timer=setTimeout(()=>{waiters.delete(id);reject(Error(`Native ${method} timeout`))},115000);waiters.set(id,{resolve,reject,timer});proc.stdin.write(JSON.stringify({id,method,...args})+'\n');});
  await Promise.race([ready,pause(20000).then(()=>{throw Error(`Native startup timeout: ${stderr}`)})]);
  const {chromium}=require(resolve('bench/operator-gui-deps/node_modules/playwright'));
  const extension=resolve('extensions/nova-chrome');
  browser=await chromium.launchPersistentContext(join(root,'chromium-profile'),{channel:'chromium',headless:false,viewport:{width:1000,height:720},args:[`--disable-extensions-except=${extension}`,`--load-extension=${extension}`,'--no-sandbox','--window-position=0,0','--window-size=1000,850','--disable-dev-shm-usage']});
  report.environment.browser=browser.browser()?.version()??'persistent Chromium';
  let worker=browser.serviceWorkers()[0];if(!worker)worker=await browser.waitForEvent('serviceworker',{timeout:15000});report.extension=worker.url();
  const page=browser.pages()[0]??await browser.newPage();await page.goto(`${origin}/app?case=form`);await page.bringToFront();
  let connected=false;
  for(let i=0;i<40;i++){
   const status=await request('native',{channel:'chrome',args:{operation:'status'}});if(status.connected){connected=true;break;}
   if(i%5===0)await worker.evaluate(()=>chrome.runtime.sendMessage({type:'reconnect'})).catch(()=>{});
   await pause(1000);
  }
  if(!connected)throw Error('Production Chrome extension did not pair with native bridge');
  report.setupChrome=await request('native',{channel:'chrome',args:{operation:'tabs'}});
  report.setupDesktop=await request('native',{channel:'jianlai',args:{operation:'windows'}});
  const snap=await request('native',{channel:'jianlai',args:{operation:'screenshot',maxEdge:1400}});report.setupScreenshot={snapshotId:snap.snapshotId,images:snap.images};
  await page.screenshot({path:join(outDir,'setup-browser.png')});
  if(setupOnly){report.status='native_setup_passed';report.nativeStderr=stderr;return report;}
  let n=0;
  for(const channel of ['chrome','jianlai'])for(const test of cases){
   const arms=n++%2?['B','A']:['A','B'];
   for(const arm of arms){
    const id=`${channel}-${test.id}-${arm}`;active={id,arm,history:[],modelRequests:0};
    const row={id,arm,channel,case:test.id,status:'starting',oraclePassed:false,actualTaskSuccess:false};report.runs.push(row);
    await page.goto(`${origin}/app?case=${test.id}`);await page.bringToFront();await pause(600);
    const tabs=await request('native',{channel:'chrome',args:{operation:'tabs'}});const tab=tabs.tabs.find(t=>t.url===page.url());if(!tab)throw Error('Test tab not found');
    const target=channel==='chrome'?{tabTag:tab.tag}:{};
    await page.screenshot({path:join(outDir,`${id}-before.png`)});
    const {scope}=await request('bind',{run:id});const start=performance.now();const nativeStart=report.nativeEvents.length;const modelStart=report.decisions.length;
    try{
     let result=await request('operate',{scope,args:{op:'run',requestKey:id,channel,target,goal:test.goal,acceptance:test.acceptance,constraints:['Operate only the Nova Native Control Test application','All changes are local and reversible; never navigate to another service','Only use the specified channel','Use fresh screenshots after scrolling or layout changes']}});
     row.phases=[result];
     for(let phase=1;result.status==='yielded'&&phase<3;phase++){result=await request('operate',{scope,args:{op:'resume',taskId:result.taskId}});row.phases.push(result);}
     row.result=result;row.status=result.status;
    }catch(e){row.status='error';row.error=redact(e.message,apiKey);}
    row.elapsedMs=performance.now()-start;
    row.oracle=await page.evaluate(()=>window.__oracle());
    row.oraclePassed=test.verify(row.oracle,row.result?.result);
    row.trustedInputEvents=row.oracle.events.filter(e=>e.trusted).length;
    row.nativeCalls=report.nativeEvents.length-nativeStart;
    row.actCalls=report.nativeEvents.slice(nativeStart).filter(e=>e.args.operation==='act').length;
    row.modelCalls=report.decisions.length-modelStart;
    row.actualTaskSuccess=row.status==='completed'&&row.oraclePassed&&row.trustedInputEvents>0;
    row.falseCompletion=row.status==='completed'&&!row.oraclePassed;
    await page.screenshot({path:join(outDir,`${id}-after.png`)});
    console.log(JSON.stringify({run:id,status:row.status,oraclePassed:row.oraclePassed,actualTaskSuccess:row.actualTaskSuccess,actCalls:row.actCalls,modelCalls:row.modelCalls,elapsedMs:Math.round(row.elapsedMs)}));
    await save();active=null;
   }
  }
  report.status='completed';report.nativeStderr=stderr;
 }catch(e){report.status='blocked';report.error=redact(e.message,apiKey);console.error(report.error);}
 finally{
  report.completedAt=new Date().toISOString();
  report.nativeActionCalls=report.nativeEvents.filter(e=>e.args?.operation==='act').length;
  report.businessSuccess=report.runs.filter(r=>r.actualTaskSuccess).length;report.falseCompletions=report.runs.filter(r=>r.falseCompletion).length;
  await save();
  // Preserve actual production observation screenshots/evidence, not credentials/profile.
  const {cp}=await import('node:fs/promises');for(const name of ['operator','desktop-shots','browser-shots'])await cp(join(data,name),join(outDir,`native-${name}`),{recursive:true}).catch(()=>{});
  await browser?.close().catch(()=>{});
  if(proc&&!stopped){proc.stdin.end(JSON.stringify({id:++serial,method:'close'})+'\n');await pause(1000);proc.kill('SIGTERM');}
  server?.closeAllConnections();await new Promise(r=>server?server.close(r):r());await rm(root,{recursive:true,force:true});
 }
 return report;
}
if(process.argv[1]&&import.meta.url===pathToFileURL(process.argv[1]).href){
 const report=await runGui({apiKey:process.env.COMMAND_CODE_API_KEY,setupOnly:process.argv.includes('--setup-only'),outDir:process.env.OPERATOR_GUI_OUT||'validation/gui'});
 console.log(JSON.stringify({status:report.status,runs:report.runs.length,passed:report.businessSuccess,modelCalls:report.modelCallsAttempted,actualNativeActions:report.nativeActionCalls}));
 if(report.status==='blocked')process.exitCode=1;
}
