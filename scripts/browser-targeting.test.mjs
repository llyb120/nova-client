// Real Chromium + isolated-world CDP tests. No page APIs, hit tests or input events are mocked.
import assert from 'node:assert/strict';
import { readFile, writeFile } from 'node:fs/promises';
import { chromium } from 'playwright-core';
const source = await readFile('src-tauri/src/native_browser_page.js','utf8');
const baselinePath = process.env.BROWSER_BASELINE || process.env.BROWSER_PAGE_BASELINE;
const baseline = baselinePath ? await readFile(baselinePath,'utf8') : null;
const browser = await chromium.launch({executablePath:process.env.TEST_BROWSER || '/usr/bin/chromium',headless:true,args:['--no-sandbox']});
const reports=[];
try {
  for (const dpr of [1,1.25,2]) {
    const context=await browser.newContext({viewport:{width:1000,height:800},deviceScaleFactor:dpr});
    const page=await context.newPage();
    const client=await context.newCDPSession(page);
    await page.setContent(`<style>body{margin:0}button,input{height:32px;margin:8px}canvas{margin:20px;border:4px solid gray;width:400px;height:200px}.scroll{height:110px;width:300px;overflow:auto}</style>
      <span id="order-label">完整订单号</span><input id="order" aria-labelledby="order-label"><input id="other" aria-label="其他字段">
      <button id="run">查询</button><fieldset disabled><button id="disabled">不可用</button></fieldset>
      <div role="row" id="row" style="width:400px;height:80px;background:#ddd"><span>订单 A</span><button id="delete">删除</button></div>
      <div class="scroll"><div style="height:1000px;padding-top:400px"><button id="nested">滚动内目标</button></div></div>
      <div id="host"></div><canvas id="paint" width="1600" height="800" aria-label="画布工作区"></canvas>`);
    await page.evaluate(()=>{
      window.clicks=[];document.addEventListener('click',e=>clicks.push({id:e.target.id,trusted:e.isTrusted,x:e.clientX,y:e.clientY}));
      const root=document.querySelector('#host').attachShadow({mode:'open'});root.innerHTML='<span id="shadow-label">阴影按钮</span><button aria-labelledby="shadow-label" id="shadow-button">fallback</button>';
      const canvas=document.querySelector('canvas'),ctx=canvas.getContext('2d');ctx.fillStyle='white';ctx.fillRect(0,0,1600,800);ctx.fillStyle='red';ctx.fillRect(600,300,100,100);
      window.canvasEvents=[];for(const type of ['pointerdown','pointermove','pointerup','dblclick','wheel'])canvas.addEventListener(type,e=>canvasEvents.push({type,trusted:e.isTrusted,x:e.clientX,y:e.clientY,buttons:e.buttons,deltaX:e.deltaX}));
    });
    const tree=await client.send('Page.getFrameTree');
    const world=await client.send('Page.createIsolatedWorld',{frameId:tree.frameTree.frame.id,worldName:'nova-target-test'});
    const evaluate=async expression=>{
      const result=await client.send('Runtime.evaluate',{expression,contextId:world.executionContextId,returnByValue:true,awaitPromise:true});
      if(result.exceptionDetails)throw Error(result.exceptionDetails.exception?.description || result.exceptionDetails.text);
      return result.result.value;
    };
    await evaluate(source);
    const observe=()=>evaluate(`__novaWebview.observe(${JSON.stringify(crypto.randomUUID())})`);
    let obs=await observe();
    const item=(name,o=obs)=>{const i=o.items.find(i=>i.name===name && i.role!=='scroll-container');assert.ok(i,`missing target: ${name}`);return i;};
    const prepare=ref=>evaluate(`__novaWebview.prepare(${JSON.stringify(ref)})`);
    const validate=(ref,p)=>evaluate(`__novaWebview.validate(${JSON.stringify(ref)},${JSON.stringify(p)})`);
    const click=async(ref)=>{const p=await prepare(ref);await client.send('Input.dispatchMouseEvent',{type:'mouseMoved',x:p.x,y:p.y});await validate(ref,p);await client.send('Input.dispatchMouseEvent',{type:'mousePressed',x:p.x,y:p.y,button:'left',clickCount:1});await client.send('Input.dispatchMouseEvent',{type:'mouseReleased',x:p.x,y:p.y,button:'left',clickCount:1});return p;};
    assert.equal(item('完整订单号').role,'input');
    assert.equal(item('不可用').disabled,true);
    await assert.rejects(prepare(item('不可用').ref),/不可用/);
    assert.equal(item('画布工作区').visual.bitmapWidth,1600);assert.equal(obs.visualRequired,true);
    await click(item('阴影按钮').ref);
    assert.ok(await page.evaluate(()=>clicks.at(-1).trusted));
    await click(item('滚动内目标').ref);
    assert.equal(await page.evaluate(()=>clicks.at(-1).id),'nested');
    obs=await observe();
    const stale=item('删除').ref;
    await page.evaluate(()=>document.querySelector('#row span').textContent='订单 B');
    await assert.rejects(prepare(stale),/引用已失效/);
    obs=await observe();
    const p=await prepare(item('查询').ref);
    await page.evaluate(()=>{const b=document.querySelector('#run');b.onmouseenter=()=>{b.style.transform='translateX(120px)';};});
    await client.send('Input.dispatchMouseEvent',{type:'mouseMoved',x:p.x,y:p.y});
    await assert.rejects(validate(item('查询').ref,p),/变化/);
    await page.evaluate(()=>{const b=document.querySelector('#run');b.onmouseenter=null;b.style.transform='';});
    obs=await observe();
    const input=item('完整订单号');await click(input.ref);
    await evaluate(`__novaWebview.focused(${JSON.stringify(input.ref)})`);
    await page.evaluate(()=>document.querySelector('#other').focus());
    await assert.rejects(evaluate(`__novaWebview.focused(${JSON.stringify(input.ref)})`),/焦点/);
    obs=await observe();
    const wait=item('不可用').ref;
    await page.evaluate(()=>setTimeout(()=>document.querySelector('fieldset').disabled=false,100));
    assert.equal((await evaluate(`__novaWebview.waitFor(${JSON.stringify(wait)},'enabled','',500)`)).matched,true);
    // Canvas receives native mouse events; physical screenshot pixels -> viewport CSS at each DPR.
    obs=await observe();const canvas=item('画布工作区');
    const region={x:canvas.viewportRect.x+10,y:canvas.viewportRect.y+10,width:380,height:180};
    const shot=await client.send('Page.captureScreenshot',{format:'png',fromSurface:true,captureBeyondViewport:false,clip:{...region,scale:1}});
    const png=Buffer.from(shot.data,'base64'),pixelWidth=png.readUInt32BE(16),pixelHeight=png.readUInt32BE(20);
    const x=region.x+pixelWidth*.5*region.width/pixelWidth,y=region.y+pixelHeight*.5*region.height/pixelHeight;
    const coord=await evaluate(`__novaWebview.coordinate(${x},${y},${JSON.stringify(obs.stamp)},false)`);
    for(const count of [1,2]){await client.send('Input.dispatchMouseEvent',{type:'mousePressed',x:coord.x,y:coord.y,button:'left',clickCount:count});await client.send('Input.dispatchMouseEvent',{type:'mouseReleased',x:coord.x,y:coord.y,button:'left',clickCount:count});}
    await client.send('Input.dispatchMouseEvent',{type:'mousePressed',x,y,button:'left',buttons:1,clickCount:1});
    for(let i=1;i<=8;i++)await client.send('Input.dispatchMouseEvent',{type:'mouseMoved',x:x+i*10,y,button:'left',buttons:1});
    await client.send('Input.dispatchMouseEvent',{type:'mouseReleased',x:x+80,y,button:'left',buttons:0,clickCount:1});
    await client.send('Input.dispatchMouseEvent',{type:'mouseWheel',x,y,deltaX:40,deltaY:0});
    await page.waitForTimeout(50);
    const events=await page.evaluate(()=>canvasEvents);
    assert.ok(events.some(e=>e.type==='dblclick'));assert.ok(events.filter(e=>e.type==='pointermove'&&e.buttons===1).length>=8);assert.ok(events.every(e=>e.trusted));assert.ok(events.some(e=>e.type==='wheel'&&e.deltaX===40));
    await page.evaluate(()=>{document.body.style.height='4000px';scrollTo(0,100);});
    await assert.rejects(evaluate(`__novaWebview.coordinate(${x},${y},${JSON.stringify(obs.stamp)},false)`),/视口已变化/);
    await page.evaluate(()=>{scrollTo(0,0);document.body.insertAdjacentHTML('beforeend',`<div id="extra" style="position:fixed;inset:0;z-index:20;background:white"><div id="transparent"><button id="hidden-target">隐藏按钮</button></div><input readonly aria-label="只读字段"><input type="range" aria-label="滑动数值"><iframe id="scaled-frame" style="position:absolute;left:50px;top:250px;width:200px;height:100px;border:4px solid;transform-origin:0 0;transform:scale(1.5,1.25)"></iframe></div>`);});
    obs=await observe();
    const hidden=item('隐藏按钮');
    await page.evaluate(()=>document.querySelector('#transparent').style.opacity='0');
    await assert.rejects(prepare(hidden.ref),/不可见/);
    assert.equal((await prepare(item('只读字段').ref)).editable,false);
    assert.equal((await prepare(item('滑动数值').ref)).editable,false);
    await assert.rejects(evaluate("__novaWebview.waitFor('invented','hidden','',0)"),/引用不存在/);
    let mapped=await evaluate("__novaWebview.frameOwner(document.querySelector('#scaled-frame'),{x:100,y:50})");
    assert.deepEqual(mapped,{x:206,y:317.5});
    await page.evaluate(()=>document.querySelector('#scaled-frame').style.transform='rotate(5deg)');
    await assert.rejects(evaluate("__novaWebview.frameOwner(document.querySelector('#scaled-frame'),{x:100,y:50})"),/旋转/);
    await page.evaluate(()=>document.querySelector('#scaled-frame').style.transform='scale(1.5,1.25)');
    await page.evaluate(()=>document.querySelector('#extra').insertAdjacentHTML('beforeend','<div style="position:absolute;left:150px;top:300px;width:150px;height:70px;background:red"></div>'));
    await assert.rejects(evaluate("__novaWebview.frameOwner(document.querySelector('#scaled-frame'),{x:100,y:50})"),/遮挡/);
    await page.evaluate(()=>document.querySelector('#extra').remove());
    reports.push({dpr,checks:'labels,disabled,shadow,nested-scroll,recycled-row,hover-move,focus-theft,wait-condition,canvas-doubleclick-drag-wheel',canvasPixels:[pixelWidth,pixelHeight],trustedCanvasEvents:events.length});
    await context.close();
  }
  const page=await browser.newPage({viewport:{width:1280,height:800}});
  await page.setContent('<style>.grid{display:grid;grid-template-columns:repeat(10,1fr)}</style><div class="grid"></div>');
  await page.evaluate(()=>{document.querySelector('.grid').innerHTML=Array.from({length:10000},(_,i)=>`<div><button>Item ${i}</button></div>`).join('');});
  async function bench(script){await page.evaluate(script);const times=[];let last;for(let i=0;i<6;i++){last=await page.evaluate(()=>__novaWebview.observe(String(Math.random())));if(i)times.push(last.timings?.observeMs || 0);}return {times,last};}
  let before;
  if(baseline){await page.evaluate(baseline);const times=[];for(let i=0;i<6;i++){const value=await page.evaluate(()=>{const t=performance.now();const o=__novaWebview.observe(String(Math.random()));return {ms:performance.now()-t,items:o.items.length};});if(i)times.push(value.ms);}before=times;}
  await page.evaluate(()=>delete globalThis.__novaWebview);
  const after=await bench(source);assert.equal(after.last.items.length,10000);
  const targeted=[];for(let i=0;i<5;i++){const result=await page.evaluate(()=>__novaWebview.observe(String(Math.random()),20000,'Item 9999'));assert.equal(result.items.length,1);targeted.push(result.timings.observeMs);}
  reports.push({benchmark:'10000 loaded buttons',baselineMs:before,optimizedMs:after.times,targetedQueryMs:targeted,items:after.last.items.length});
  console.log(JSON.stringify(reports,null,2));
  await writeFile(process.env.BROWSER_REPORT || 'browser-targeting-report.json',JSON.stringify(reports,null,2));
} finally {await browser.close();}
