// Opt-in real-site A/B runner. Each invocation owns one Nova profile and one Chrome tab.
// node scripts/jev-databrain-ab.mjs --run <label> <exe> <on|off> [--require-jev]
import {readFile,writeFile,mkdtemp,mkdir,unlink} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {tmpdir} from 'node:os';
import {spawn} from 'node:child_process';
import {createServer} from 'node:http';
import {createHash} from 'node:crypto';
import {reportThread} from './jev-session-report.mjs';
if(process.argv[2]!=='--run')throw Error('Explicit --run required for live model/site requests');
const [label,exe,mode]=process.argv.slice(3);
const requireJev=process.argv[6]==='--require-jev';
const sortOnly=process.argv.includes('--sort-only');
const regionRegression=process.argv.includes('--region-regression');
if(!/^[a-z0-9-]+$/.test(label)||!exe||!['on','off'].includes(mode))throw Error('Invalid arguments');
const root=process.cwd(),out=resolve('src-tauri/target/jev-ab',label);
await mkdir(out,{recursive:true});
const profile=await mkdtemp(join(tmpdir(),'nova-databrain-ab-'));
const source=JSON.parse(await readFile(join(process.env.USERPROFILE,'.novadev/settings.json'),'utf8'));
await writeFile(join(profile,'settings.json'),JSON.stringify({
  jevEnabled:mode==='on',jevApiKey:mode==='on'?source.jevApiKey:'',relayServer:'',relayToken:'',sessionShortcuts:[],
  lyraEnabled:false,codebuddyEnabled:true,codexEnabled:false,codebuddyPath:source.codebuddyPath,
  codebuddyArgs:source.codebuddyArgs,codebuddyProxy:source.codebuddyProxy,codebuddyIntegration:source.codebuddyIntegration,
}));
const listener=createServer();await new Promise(r=>listener.listen(0,'127.0.0.1',r));
const port=listener.address().port;await new Promise(r=>listener.close(r));
const child=spawn(resolve(exe),[],{cwd:root,windowsHide:true,env:{...process.env,NOVA_DATA_DIR:profile,
  WEBVIEW2_USER_DATA_FOLDER:join(profile,'webview-runtime'),
  WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:`--remote-debugging-port=${port} --remote-debugging-address=127.0.0.1 --disable-features=CalculateNativeWinOcclusion`},stdio:'ignore'});
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
let socket,invoke,thread,tag,latest;
const report={label,exe:resolve(exe),exeSha256:createHash('sha256').update(await readFile(exe)).digest('hex'),
  jevEnabled:mode==='on',requireJev,model:'deepseek-v4.1-flash:high',profile,pid:child.pid,port};
const save=()=>writeFile(join(out,'report.json'),JSON.stringify(report,null,2));
try {
  let target;
  for(let n=0;n<100;n++){try{target=(await(await fetch(`http://127.0.0.1:${port}/json/list`)).json()).find(t=>t.type==='page' && /^https?:/.test(t.url));if(target)break;}catch{}await sleep(300);}
  if(!target)throw Error('Dev debug target unavailable');report.devUrl=target.url;
  socket=new WebSocket(target.webSocketDebuggerUrl);await new Promise((r,j)=>{socket.onopen=r;socket.onerror=j;});
  let seq=0;const pending=new Map();
  socket.onmessage=({data})=>{const m=JSON.parse(data),p=pending.get(m.id);if(p){clearTimeout(p.timer);pending.delete(m.id);m.error?p.reject(Error(JSON.stringify(m.error))):p.resolve(m.result);}};
  const evaluate=expression=>new Promise((resolve,reject)=>{const id=++seq;const timer=setTimeout(()=>{pending.delete(id);reject(Error('invoke timeout'));},240000);
    pending.set(id,{timer,reject,resolve:r=>r.exceptionDetails?reject(Error(r.exceptionDetails.exception?.description||String(r.exceptionDetails.exception?.value||r.exceptionDetails.text))):resolve(r.result.value)});
    socket.send(JSON.stringify({id,method:'Runtime.evaluate',params:{expression,returnByValue:true,awaitPromise:true}}));});
  for(let n=0;n<100;n++){if(await evaluate('!!window.__TAURI_INTERNALS__&&!!document.querySelector(".app")'))break;await sleep(200);}
  if(!await evaluate('!!document.querySelector(".app")'))throw Error('Dev frontend did not become ready');
  invoke=(command,args={})=>evaluate(`window.__TAURI_INTERNALS__.invoke(${JSON.stringify(command)},${JSON.stringify(args)})`);
  const chrome=(operation,args={})=>invoke('chrome_browser_ui',{operation,args});
  thread=await invoke('create_thread',{cwd:'D:\\code\\intelligence-pc-backend',agentKind:'codebuddy',model:report.model,mode:'build',ephemeral:false});
  report.threadId=thread.id;
  for(let n=0;n<50;n++){if(await evaluate('!!document.querySelector(".thread-item")'))break;await sleep(200);}
  await evaluate('document.querySelector(".thread-item").click();true');
  await invoke('report_activity',{threadId:thread.id});
  let status;
  for(let n=0;n<40;n++){status=await chrome('connect');if(status.connected)break;await sleep(1000);}
  if(!status.connected)throw Error('Chrome extension did not connect');
  // ACP may materialize the embedded bundle only when the first prompt starts.
  report.connectionOrigin=status.origin;
  const opened=await chrome('new_tab',{url:'http://databrain-test.intlgame.com/'+(sortOnly?'v2/intelligence/topCharts/pcConsoleGames/MetricsTable':'')});
  tag=opened.tabTag||opened.tag;if(!tag)throw Error('No owned test tab');report.tabTag=tag;
  await sleep(2500);
  report.initial=await chrome('inspect',{tabTag:tag,scope:'viewport',maxTextChars:4000});
  await writeFile(join(out,'initial.json'),JSON.stringify(report.initial,null,2));
  if(/login|sign.?in/i.test(report.initial.pages?.[0]?.url||''))throw Error('Test tab is not logged in');
  if(process.argv.includes('--dom-probe')) {
    const filters=process.argv.includes('--filters')||regionRegression;
    report.testKind='direct_jev_navigation_only';report.startedAt=Date.now();report.probe=[];
    let observed=report.initial;
    for(let attempt=0;attempt<(filters||sortOnly?1:4);attempt++) {
      const before=JSON.stringify(observed.pages?.map(p=>[p.url,p.text]));
      const result=await chrome('run',{tabTag:tag,snapshotId:observed.snapshotId,plan:{
        task:filters?'进入 Intelligence / Top Charts / PC & Console Games 的 Metrics Table，日期选择 Last 26 Weeks（最新完整周为2026-09-19），Region选择United States并Confirm应用筛选，再把表格的Digital Units按降序排列（不是图表Metrics）。':'从首页进入 Intelligence 的 Top Charts 下 PC & Console Games 榜单（合并PC与Console的平台榜单）。',
        authorization:filters?'允许导航、打开日期预设、选择地区、搜索United States、关闭菜单、Confirm应用查询筛选、滚动和表格排序；不得保存、导出或修改业务数据。':'仅允许点击当前站点导航以进入指定榜单；不得修改筛选，不得提交或写入数据。',
        expectedText:filters?'PC & Console Games Metrics Table，日期2026-03-22~2026-09-19，Region United States，表格Digital Units降序且数据加载完成。':'PC & Console Games 榜单的 Metrics Table 和 Region 筛选已加载。',
        inputs:filters?[{name:'Search',text:'United States'}]:[],maxActions:32,
        ...(regionRegression?{task:'进入 Intelligence / Top Charts / PC & Console Games 的 Metrics Table，把 Region 从 Global 改为仅 United States，把 Week 开始和结束日期填写为2026-03-23和2026-09-23，点击 Confirm 应用。日期控件会按周归一化，允许最终为2026-03-22~2026-09-26；不要反复重填。不要选择整个地区分组。',
          authorization:'允许导航、日期填写、地区筛选、滚动和 Confirm 应用查询；禁止修改其它筛选、排序、保存或导出。',
          expectedText:'Region 仅 United States，Week 2026-03-22~2026-09-26，已 Confirm 应用且数据加载完成',
          inputs:[{name:'Week ~ [field 1/2]',role:'input',text:'2026-03-23'},{name:'Week ~ [field 2/2]',role:'input',text:'2026-09-23'}]}:{}),
        ...(sortOnly?{task:'将当前榜单表格的 MScience Digital Units（排序字段 units，不是 GSD 同名列 gsd_digitalUnits）按降序排列，不是图表Metrics。',
          authorization:'允许滚动、打开表格排序菜单和选择排序项；禁止修改筛选、导出或保存。',
          expectedText:'表格排序字段units、方向desc，数据加载完成；仅图表指标变化不算完成',inputs:[],maxActions:12}:{})
      }});
      report.probe.push(result.jevRun);await writeFile(join(out,`probe-${attempt}.json`),JSON.stringify(result,null,2));
      await sleep(1000);
      observed=await chrome('inspect',{tabTag:tag,scope:'viewport',visual:'none',maxTextChars:12000});
      const page=observed.pages?.[0];
      report.probePassed=!!page && new URL(page.url).pathname.endsWith('/intelligence/topCharts/pcConsoleGames/MetricsTable') && /Metrics Table/.test(page.text) && /Region/.test(page.text) && !/Loading\.\.\./.test(page.text);
      if(filters&&!regionRegression)report.probePassed=report.probePassed && result.jevRun?.status==='completed'
        && /United States/.test(page.text) && /2026-03-22/.test(page.text) && /2026-09-19/.test(page.text)
        && new URL(page.url).searchParams.get('sort_name')==='units' && new URL(page.url).searchParams.get('order')==='desc';
      if(regionRegression) {
        const history=result.jevRun?.history||[];
        const fills=history.filter(h=>h.action.includes('"action":"fill"'));
        report.probePassed=report.probePassed && result.jevRun?.status==='completed'
          && /Region\s+United States\s+Platform/.test(page.text)
          && /2026-03-22/.test(page.text) && /2026-09-26/.test(page.text)
          && history.some(h=>h.action.includes('"name":"Confirm"')) && fills.length===2;
      }
      if(sortOnly)report.probePassed=report.probePassed && result.jevRun?.status==='completed'
        && new URL(page.url).searchParams.get('sort_name')==='units' && new URL(page.url).searchParams.get('order')==='desc';
      console.log(JSON.stringify({phase:'dom_probe',attempt,run:result.jevRun,passed:report.probePassed}));
      if(report.probePassed || JSON.stringify(observed.pages?.map(p=>[p.url,p.text]))===before)break;
    }
    report.completed=report.probePassed;report.wallMs=Date.now()-report.startedAt;
    report.jevParticipationMet=report.probe.some(r=>r?.requestCount>0&&r?.executedActions>0);
    if(!report.probePassed||!report.jevParticipationMet)process.exitCode=1;
  } else {
  report.prompt=`http://databrain-test.intlgame.com/\n\n打开情报的pc&console的榜单页面，查询近半年美国的数据，用unit倒序排列\n\n测试环境：今天是2026-09-22；已为本次任务打开独立 Chrome 标签，tabTag=${tag}。只操作这个标签，勿读取或复用其它标签的结果。在工具支持且显示 JEV 已启用时，DOM操作默认通过run交给JEV选择，提供任务、授权、准确输入和完成条件，不要预先替JEV逐步做完决策。JEV不支持视觉；需要视觉、调用失败或DOM校验失败时主模型兜底，解决后恢复委托。`;
  if(requireJev)report.prompt+='\n本次需要测试实际协作：JEV 已启用时，至少将一段连续简单子目标交给 run 实际执行，不要先自行判断完每一步再以单步为由全部跳过 JEV；JEV 不支持时由主模型完成。忽略工具自动附带的其它标签清单、URL 和历史结果，它们不属于本次任务证据。';
  report.startedAt=Date.now();await save();
  console.log(JSON.stringify({phase:'started',label,threadId:thread.id,tag,profile}));
  await invoke('send_prompt',{threadId:thread.id,text:report.prompt});
  for(let n=0;n<120;n++){
    await sleep(5000);latest=await invoke('get_thread',{threadId:thread.id});
    if(report.runtimeHasDomPlans===undefined) {
      const runtime=await readFile(join(profile,'runtime/nova-tools-mcp.mjs'),'utf8').catch(error=>{if(error.code==='ENOENT')return null;throw error;});
      if(runtime!==null)report.runtimeHasDomPlans=runtime.includes('maxActions') && /210000|21e4/.test(runtime);
    }
    if(mode==='on'&&report.runtimeHasDomPlans===false)throw Error('Stale embedded MCP bundle: rebuild build:nova-tools-mcp before this A/B');
    await writeFile(join(out,'thread.json'),JSON.stringify(latest,null,2));
    if(n%6===0)console.log(JSON.stringify({phase:'progress',label,seconds:Math.round((Date.now()-report.startedAt)/1000),items:latest.items.length,last:latest.items.slice(-2).map(i=>({type:i.type,text:i.text?.slice(-300),tool:i.call?.rawInput?.operation}))}));
    if(latest.items.some(i=>i.type==='turn'))break;
  }
  report.wallMs=Date.now()-report.startedAt;
  report.completed=latest?.items.some(i=>i.type==='turn')||false;
  if(!report.completed) {
    await invoke('cancel_turn',{threadId:thread.id});
    await sleep(500);
    latest=await invoke('get_thread',{threadId:thread.id});
    await writeFile(join(out,'thread.json'),JSON.stringify(latest,null,2));
  }
  report.audit=latest?reportThread(latest):null;
  report.jevParticipationMet=mode==='off'||(report.audit?.requestCount>0&&report.audit?.jevExecutedActions>0);
  report.turns=latest?.items.filter(i=>i.type==='turn');
  report.finalAnswer=latest?.items.filter(i=>i.type==='assistant').at(-1)?.text;
  }
  report.finalObservation=await chrome('inspect',{tabTag:tag,scope:'all',maxTextChars:12000,maxItems:100});
  await writeFile(join(out,'final-observation.json'),JSON.stringify(report.finalObservation,null,2));
  if(report.finalObservation.documentPath)await writeFile(join(out,'final-document.json'),await readFile(report.finalObservation.documentPath));
  report.verification='pending independent evidence review';
  console.log(JSON.stringify({phase:'finished',label,wallMs:report.wallMs,audit:report.audit,answer:report.finalAnswer}));
} catch(error){report.error=String(error);report.errorStack=error.stack;process.exitCode=1;console.log(JSON.stringify({phase:'error',label,error:String(error)}));}
finally {
  if(invoke&&thread&&!report.completed)await invoke('cancel_turn',{threadId:thread.id}).catch(()=>{});
  if(invoke&&tag)await invoke('chrome_browser_ui',{operation:'close_tab',args:{tabTag:tag}}).catch(e=>report.cleanupError=String(e));
  await save();socket?.close();child.kill();child.unref();
  await unlink(join(profile,'settings.json')).catch(()=>{});
  console.log(JSON.stringify({phase:'saved',label,out,profile}));
}
