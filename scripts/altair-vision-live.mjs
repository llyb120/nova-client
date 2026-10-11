// Opt-in live check of the Altair vision proxy: starts an isolated debug Nova (copy of ~/.novadev)
// and lets a real Lyra main model use webview (and optionally jianlai) on a local canvas page.
// Requires `cargo build` (debug nova.exe) and the Vite dev server on 5173 (started here if absent).
// Run from repository root: node scripts/altair-vision-live.mjs --run [--altair-off] [--desktop|--desktop-only] [--model provider/model] [--altair provider/model]
// The desktop case needs an unlocked desktop (jianlai cannot capture the lock screen).
import assert from 'node:assert/strict';
import {readFile,writeFile,mkdtemp,mkdir,copyFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {spawn} from 'node:child_process';
import {createServer} from 'node:http';
if(process.argv[2]!=='--run')throw Error('Use --run to authorize live model requests and desktop/webview input');
const arg=name=>{const i=process.argv.indexOf(name);return i>0?process.argv[i+1]:undefined};
const model=arg('--model')||'commandcode/claude-sonnet-5-5';
const desktop=process.argv.includes('--desktop')||process.argv.includes('--desktop-only');
const altairEnabled=!process.argv.includes('--altair-off');
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
const source=join(process.env.USERPROFILE,'.novadev');
const profile=await mkdtemp(join(tmpdir(),'nova-altair-live-'));
const settings=JSON.parse(await readFile(join(source,'settings.json'),'utf8'));
if(altairEnabled)assert(settings.altairModel,'select an Altair model in NovaDev settings first');
await writeFile(join(profile,'settings.json'),JSON.stringify({relayServer:'',relayToken:'',sessionShortcuts:[],
  lyraEnabled:true,lyraProxy:settings.lyraProxy,altairEnabled,altairModel:arg('--altair')||settings.altairModel,
  codebuddyEnabled:false,codexEnabled:false}));
await mkdir(join(profile,'alkaid'));
for(const f of ['config.jsonc','models-cache.json'])await copyFile(join(source,'alkaid',f),join(profile,'alkaid',f)).catch(()=>{});

// Canvas-only page: the number and the button exist only as pixels, so DOM text cannot answer.
let clicks=0;
const fixture=createServer((req,res)=>{
  if(req.url.startsWith('/clicked')){clicks++;res.writeHead(204);res.end();return}
  res.writeHead(200,{'Content-Type':'text/html; charset=utf-8'});
  res.end(`<!doctype html><meta charset="utf-8"><title>Altair 视觉回归</title><body style="margin:0">
<canvas id="c" width="900" height="500" aria-label="仓库看板"></canvas><output id="status">尚未点击</output><script>
const c=document.getElementById('c'),g=c.getContext('2d');g.fillStyle='#f4f4f4';g.fillRect(0,0,900,500);
g.fillStyle='#222';g.font='48px sans-serif';g.fillText('库存 4271 件',60,120);
g.fillStyle='#d22';g.fillRect(560,300,220,90);g.fillStyle='#fff';g.font='40px sans-serif';g.fillText('确认',625,360);
g.fillStyle='#24c';g.fillRect(120,300,220,90);g.fillStyle='#fff';g.fillText('取消',185,360);
c.addEventListener('click',e=>{const r=c.getBoundingClientRect(),x=e.clientX-r.left,y=e.clientY-r.top;
  const hit=x>=560&&x<=780&&y>=300&&y<=390;document.getElementById('status').textContent=hit?'已点击红色确认按钮':'点到了别处';
  if(hit)fetch('/clicked');});
</script>`);
});
await new Promise(r=>fixture.listen(0,'127.0.0.1',r));
const fixtureUrl=`http://127.0.0.1:${fixture.address().port}/`;

const children=[];
const vite=await fetch('http://127.0.0.1:5173/').then(()=>true,()=>false);
if(!vite){children.push(spawn(process.platform==='win32'?'npx.cmd':'npx',['vite','--port','5173','--strictPort','--host','127.0.0.1'],{shell:true,stdio:'ignore'}));
  for(let i=0;i<60&&!await fetch('http://127.0.0.1:5173/').then(()=>true,()=>false);i++)await sleep(500);}
const port=9300+Math.floor(Math.random()*500);
const app=spawn(resolve('src-tauri/target/debug/nova.exe'),[],{env:{...process.env,NOVA_DATA_DIR:profile,
  WEBVIEW2_USER_DATA_FOLDER:join(profile,'webview-runtime'),
  WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:`--remote-debugging-port=${port} --remote-debugging-address=127.0.0.1`},stdio:'ignore'});
children.push(app);
const report={profile,model,altairEnabled,altairModel:arg('--altair')||settings.altairModel,cases:[]};
try{
  let target;
  for(let i=0;i<150&&!target;i++){target=await fetch(`http://127.0.0.1:${port}/json/list`).then(r=>r.json()).then(l=>l.find(t=>t.type==='page'&&!t.url.startsWith('http://127.0.0.1:'+fixture.address().port)),()=>null);if(!target)await sleep(400)}
  assert(target,'Dev UI debug target unavailable');
  const socket=new WebSocket(target.webSocketDebuggerUrl);await new Promise((r,j)=>{socket.onopen=r;socket.onerror=j});
  let id=0;const pending=new Map();
  socket.onmessage=({data})=>{const m=JSON.parse(data),p=pending.get(m.id);if(p){pending.delete(m.id);m.error?p.reject(Error(JSON.stringify(m.error))):p.resolve(m.result)}};
  const cdp=(method,params)=>new Promise((resolve,reject)=>{const key=++id;pending.set(key,{resolve,reject});socket.send(JSON.stringify({id:key,method,params}))});
  const evaluate=async expression=>{const r=await cdp('Runtime.evaluate',{expression,returnByValue:true,awaitPromise:true});if(r.exceptionDetails)throw Error(r.exceptionDetails.exception?.description||r.exceptionDetails.text);return r.result.value};
  for(let i=0;i<100&&!await evaluate('!!window.__TAURI_INTERNALS__&&!!document.querySelector(".app")');i++)await sleep(200);
  const invoke=(command,args={})=>evaluate(`window.__TAURI_INTERNALS__.invoke(${JSON.stringify(command)},${JSON.stringify(args)})`);

  const runCase=async(name,prompt,check)=>{
    const thread=await invoke('create_thread',{cwd:profile,agentKind:'lyra',model,mode:'build',ephemeral:false});
    for(let i=0;i<50&&!await evaluate('!!document.querySelector(".thread-item")');i++)await sleep(200);
    await evaluate(`[...document.querySelectorAll(".thread-item")].find(e=>e.closest("[data-thread-id]")?.dataset.threadId===${JSON.stringify(thread.id)})?.click() ?? document.querySelector(".thread-item").click(); true`);
    await invoke('report_activity',{threadId:thread.id});
    const started=Date.now();
    await invoke('send_prompt',{threadId:thread.id,text:prompt});
    let meta;
    for(let i=0;i<300;i++){await sleep(2000);meta=(await invoke('list_threads')).find(t=>t.id===thread.id);if(i>2&&!meta?.running)break}
    if(meta?.running)await invoke('cancel_turn',{threadId:thread.id}).catch(()=>{});
    const full=await invoke('get_thread',{threadId:thread.id});
    const tools=full.items.filter(i=>i.type==='tool');
    const outputs=tools.filter(t=>/webview|jianlai|chrome/i.test(t.title));
    const text=JSON.stringify(full.items);
    const answer=full.items.filter(i=>i.type==='assistant').map(i=>i.text||'').join('\n').slice(-1500);
    const replies=outputs.flatMap(t=>(t.rawOutput?.content||[]).flatMap(c=>{
      if(c.type!=='text')return [];try{return [JSON.parse(c.text)]}catch{return []}
    }));
    const vision=replies.filter(r=>r.vision?.by==='altair').length;
    const leakedImages=new Set(replies.flatMap(r=>[r.path,r.imagePath,...(r.images||[]).map(i=>i.path)])
      .filter(p=>typeof p==='string'&&p.length>0)).size;
    const visionErrors=replies.filter(r=>r.vision?.error).length;
    const result={name,threadId:thread.id,elapsedMs:Date.now()-started,timedOut:!!meta?.running,toolCalls:outputs.length,vision,leakedImages,visionErrors,answer,
      runs:replies.filter(r=>r.altairRun).map(r=>r.altairRun),
      disabledAdvice:replies.filter(r=>r.status==='disabled'&&r.advisoryOnly&&r.requestAttempted===false).length};
    report.cases.push({...result,itemsSample:text.length});
    console.log(JSON.stringify(result,null,1));
    await writeFile(join(profile,`${name}-thread.json`),JSON.stringify(full,null,1));
    check(result,outputs);
  };

  if(!process.argv.includes('--desktop-only')) await runCase('webview-canvas',
    `用 webview 打开 ${fixtureUrl} 。页面主体是 canvas，内容只在像素里。请：1) 告诉我 canvas 上显示的库存数字；2) 点击 canvas 上红色的「确认」按钮一次（${altairEnabled?'坐标类动作，可用 target 让系统定位':'Altair已关闭，直接看截图，用x/y坐标操作'}）；3) 读取页面 #status 的文字确认结果。这是本地测试页，允许点击。最后用一句话汇报库存数字和 #status 文字。`,
    r=>{assert(!r.timedOut);if(altairEnabled){assert(r.vision>0,'tool replies must carry vision text');assert.equal(r.leakedImages,0,'no screenshot path may reach the main model');}
       else {assert.equal(r.vision,0,'disabled Altair must not replace images');assert(r.leakedImages>0,'the main model must receive screenshots');}
       assert.match(r.answer,/4271/);assert.equal(clicks,1,'red button clicked exactly once');});
  if(!altairEnabled&&!process.argv.includes('--desktop-only')) await runCase('webview-disabled-compat',
    `这是本地兼容性回归，请严格按顺序调用 webview：1) 打开 ${fixtureUrl} 并inspect拿最新snapshotId；2) 调用advise，advice为{task:"读取状态",state:"尚未点击",choices:{ready:"尚未点击"}}，核对返回disabled；3) 调用run，携带最新snapshotId，plan为{task:"兼容性检查",authorization:"仅允许按Escape",expectedText:"尚未点击"}，故意不传steps，核对返回handoff且未执行；4) 用刚返回的最新snapshotId再run，同一plan加steps:[{action:"press",key:"Escape"}]，核对完成且只执行一步；5) 保存最新snapshotId后调用stop；6) 故意用刚保存的snapshotId再次提交第4步run，应当被拒绝，不要重新inspect或重试。不要点击画布按钮。最后报告三次run的状态。`,
    (r,outputs)=>{assert(!r.timedOut);assert.equal(r.vision,0);assert.equal(r.disabledAdvice,1);
       assert.equal(r.runs.length,2);assert.equal(r.runs[0].status,'handoff');assert.equal(r.runs[0].executedActions,0);
       assert.equal(r.runs[1].status,'completed');assert.equal(r.runs[1].executedActions,1);
       assert.equal(r.runs[1].verification,'subgoal_verified');assert(r.runs.every(run=>run.requestCount===0));
       const calls=outputs.filter(t=>t.rawInput?.operation==='run');assert.equal(calls.length,3);
       assert.match(JSON.stringify(calls[2].rawOutput),/观察已失效/,'explicit stop must still invalidate the snapshot');});
  if(desktop) await runCase('desktop-notepad',
    '用剑来（jianlai）打开记事本：Win+R，输入 notepad，回车。在记事本文本区输入「Altair 视觉测试 123」，然后截图确认文字已经出现，告诉我记事本窗口标题和文本区内容。不要保存，不要关闭窗口。',
    r=>{assert(!r.timedOut);if(altairEnabled){assert(r.vision>0);assert.equal(r.leakedImages,0);}else{assert.equal(r.vision,0);assert(r.leakedImages>0);}assert.match(r.answer,/Altair 视觉测试 123/);
       assert.doesNotMatch(r.answer,/没能|无法确认|没有输入|失败/,'the model must report success, not quote the text in a failure');});
  report.passed=true;
}catch(error){report.error=String(error?.stack||error);console.error(report.error)}
finally{
  await writeFile(join(profile,'report.json'),JSON.stringify(report,null,1));
  console.log(JSON.stringify({phase:'done',passed:!!report.passed,profile}));
  for(const child of children.reverse())child.kill();
  fixture.close();
  process.exit(report.passed?0:1);
}
