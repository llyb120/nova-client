// Real Chromium + the production isolated-world engine; Playwright is only a test driver.
// TEST_BROWSER selects a local Chromium/Chrome/Edge. No model or external site is used.
import assert from 'node:assert/strict';
import { readFile, writeFile, access } from 'node:fs/promises';
import { randomUUID, createHash } from 'node:crypto';
import { chromium } from 'playwright-core';
const engine = await readFile(new URL('../src-tauri/src/native_browser_page.js', import.meta.url), 'utf8');
const report = { cases: [], benchmarks: {}, node:process.version, engineSha256:createHash('sha256').update(engine).digest('hex'), scope:'Production page script and iframe mapping in real Chromium/CDP; not the compiled Rust/Tauri dispatch path.' };
let browser;
const run = async (name, fn) => {
  const t=performance.now();
  try { await fn(); report.cases.push({name,passed:true,ms:Math.round((performance.now()-t)*10)/10});console.log(`PASS ${name}`); }
  catch(error) {
    if(error.code==='UNSUPPORTED_WEBGL' && process.env.REQUIRE_WEBGL!=='1') {report.cases.push({name,passed:false,skipped:true,reason:error.message});console.log(`SKIP ${name}: ${error.message}`);}
    else {report.cases.push({name,passed:false,error:String(error)});throw error;}
  }
};
try {
  let executablePath;
  for (const p of [process.env.TEST_BROWSER, '/usr/bin/chromium', '/usr/bin/google-chrome', 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe'].filter(Boolean)) {
    try { await access(p); executablePath=p;break; } catch {}
  }
  browser = await chromium.launch({executablePath, headless:true, args:['--no-sandbox']});
  report.browser=browser.version();
  const context=await browser.newContext({viewport:{width:900,height:700},deviceScaleFactor:2});
  const page=await context.newPage();
  const cdp=await context.newCDPSession(page);
  let world;
  const evaluate=async expression=> {
    const r=await cdp.send('Runtime.evaluate',{expression,contextId:world,returnByValue:true,awaitPromise:true});
    if(r.exceptionDetails)throw Error(r.exceptionDetails.exception?.description||r.exceptionDetails.text);
    return r.result.value;
  };
  const call=(method,...args)=>evaluate(`__novaWebview.${method}(${args.map(v=>JSON.stringify(v)).join(',')})`);
  const fixture=async html=>{
    await page.setContent(`<style>body{margin:0;font:16px Arial}button,input{padding:9px}section{padding:10px}</style>${html}`);
    const {frameTree}=await cdp.send('Page.getFrameTree');
    world=(await cdp.send('Page.createIsolatedWorld',{frameId:frameTree.frame.id,worldName:'nova-precision-test'})).executionContextId;
    await evaluate(engine);
    return call('observe',randomUUID());
  };
  const target=(obs,name)=> {const e=obs.items.find(e=>e.name===name);assert.ok(e,`missing ${name}`);return e;};
  await run('custom menus and duplicate sortable columns retain independent evidence',async()=>{
    const obs=await fixture('<nav><div style="cursor:pointer"><span>PC & Console</span></div></nav><div>Unrelated</div><div style="cursor:pointer">Custom option</div><div role="menu"><div style="cursor:pointer">United States</div></div><table><tr><th style="cursor:pointer" data-field="sourceA" aria-sort="descending">Digital Units</th><th style="cursor:pointer" data-field="sourceB" aria-sort="none">Digital Units</th><th>Revenue</th></tr><tr><td>100</td><td>50</td><td>99</td></tr></table>');
    assert.equal(target(obs,'PC & Console').actionable,true);
    assert.equal(target(obs,'United States').actionable,true);
    assert.equal(target(obs,'Custom option').actionable,true);
    assert.ok(!obs.items.some(i=>i.name==='Unrelated'));
    const headers=obs.items.filter(i=>i.name==='Digital Units');assert.equal(headers.length,2);
    assert.notDeepEqual(headers[0].column,headers[1].column);
    assert.equal(headers[0].sort,'descending');assert.equal(headers[1].sort,'none');
    assert.equal(obs.tables[0].columns[1].key,'sourceB');
    assert.equal(obs.tables[0].columns[1].ref,headers[1].ref);
    assert.equal(target(obs,'Revenue').actionable,false);
    await page.evaluate(()=>document.querySelector('th').setAttribute('aria-sort','ascending'));
    await assert.rejects(call('prepare',headers[0].ref),/失效/,'a changed sort state must invalidate the old target');
  });
  await run('DOM hint identities survive observations but not node replacement',async()=>{
    const first=await fixture('<button>Open</button><button>Open</button><input aria-label="Name">');
    const second=await call('observe',randomUUID());
    assert.deepEqual(first.items.map(i=>i.nodeId),second.items.map(i=>i.nodeId));
    assert.notEqual(first.items[0].ref,second.items[0].ref);
    const twins=second.items.filter(i=>i.name==='Open');assert.equal(twins.length,2);
    assert.notEqual(twins[0].nodeId,twins[1].nodeId);
    await page.evaluate(()=>{const old=document.querySelector('button');old.replaceWith(old.cloneNode(true));});
    const replaced=await call('observe',randomUUID());
    assert.notEqual(replaced.items[0].nodeId,first.items[0].nodeId);
    assert.equal(replaced.items[1].nodeId,first.items[1].nodeId);
  });
  await run('hidden and offscreen bulk DOM cannot exhaust visible custom menu hints',async()=>{
    const bulk='<span>Cell</span>'.repeat(4100);
    const invisible='<span style="position:fixed;top:0;visibility:hidden">Hidden</span>'.repeat(4100);
    const obs=await fixture(`<div style="display:none">${bulk}</div><div style="position:absolute;top:1200px">${bulk}</div>${invisible}<div style="position:fixed;top:20px;left:20px;cursor:pointer">Last 26 Weeks</div>`);
    assert.equal(target(obs,'Last 26 Weeks').actionable,true);
    assert.equal(obs.customTargetsTruncated,false);
  });
  await run('control hints exclude inherited pointer labels and semantic wrappers',async()=>{
    const obs=await fixture('<button style="cursor:pointer"><span>Save</span><span>Draft</span></button><div style="cursor:pointer"><span>Open</span><span>Menu</span></div><div style="cursor:pointer"><button>Submit</button></div><span role="button"><button>Nested</button></span><div tabindex="0">Focus region</div>');
    const hints=obs.items.filter(i=>i.actionable || i.role==='button');
    assert.deepEqual(hints.map(i=>i.name).sort(),['Nested','OpenMenu','SaveDraft','Submit'].sort());
    assert.equal(target(obs,'Focus region').actionable,false);
  });
  await run('nested menus retain leaf choices instead of hinting their whole group',async()=>{
    const obs=await fixture('<ul style="cursor:pointer"><li>Top Charts<ul><li><span>Mobile Games</span></li><li><span>PC &amp; Console Games</span></li></ul></li></ul>');
    const hints=obs.items.filter(i=>i.actionable);
    assert.deepEqual(hints.map(i=>i.name).sort(),['Mobile Games','PC & Console Games'].sort());
    assert.ok(hints.every(i=>i.tag==='li'));
  });
  const click=async item=>{
    const p=await call('prepare',item.ref);
    await cdp.send('Input.dispatchMouseEvent',{type:'mouseMoved',x:p.x,y:p.y});
    await call('verifyPoint',item.ref,p.x,p.y,p.rect);
    for (const type of ['mousePressed','mouseReleased'])await cdp.send('Input.dispatchMouseEvent',{type,x:p.x,y:p.y,button:'left',clickCount:1});
  };
  await run('clickable children of duplicate headers retain their owning column',async()=>{
    const obs=await fixture('<table><tr><th aria-description="Source A"><span style="cursor:pointer">Digital Units</span></th><th aria-description="Source B"><span style="cursor:pointer">Digital Units</span></th></tr></table>');
    const children=obs.items.filter(i=>i.role==='span'&&i.name==='Digital Units');
    assert.equal(children.length,2);
    for(const child of children) {
      assert.equal(child.actionable,true);
      const header=obs.items.find(i=>i.role==='th'&&i.column.index===child.column.index);
      assert.deepEqual(child.column,header.column);
    }
    assert.notDeepEqual(children[0].column,children[1].column);
  });
  await run('Top 5 survives Metrics 4, offscreen rows and explicit pagination totals',async()=>{
    const names=['Fortnite','NBA 2K27','NBA 2K26','Grand Theft Auto VI','Tomodachi Life: Living the Dream'];
    const obs=await fixture(`<p>United States · 2026-03-22 ~ 2026-09-19 · Digital Revenue descending</p><h3>Metrics 4</h3><table><tr><th>Rank</th><th>Game</th><th>Revenue</th></tr>${Array.from({length:50},(_,i)=>`<tr><td>${i+1}</td><td>${names[i]||'Game '+(i+1)}</td><td>${i===4?'145.6M':'100M'}</td></tr>`).join('')}</table><p>1691 total items</p>`);
    const table=obs.tables[0];
    assert.equal(table.totalRows,1691);assert.equal(table.loadedRows,50);assert.equal(table.returnedRows,10);
    assert.equal(table.rows[4][1],names[4]);assert.equal(table.rows[4][2],'145.6M');
    assert.equal(table.previewTruncated,true);assert.equal(table.moreRows,true);
    await page.evaluate(()=>document.querySelector('p:last-child').remove());
    assert.equal((await call('observe',randomUUID())).tables[0].totalRows,null,'Metrics 4 must never become a total');
  });
  await run('table previews exclude nested rows and do not assign ambiguous page totals',async()=>{
    const obs=await fixture('<table><tr><td>Outer<table><tr><td>Inner</td></tr></table></td></tr></table><p>30 total items</p><div role="grid" aria-rowcount="101"><div role="row"><span role="columnheader">Name</span></div><div role="row"><span role="gridcell">Virtual row</span></div></div>');
    assert.equal(obs.tables[0].loadedRows,1);assert.equal(obs.tables[0].totalRows,null);
    assert.equal(obs.tables[2].totalRows,100);assert.equal(obs.tables[2].moreRows,true);
    const hidden=await fixture('<table aria-rowcount="-1"><tr hidden><td>Old result</td></tr><tr><td>Current result</td></tr></table>');
    assert.equal(hidden.tables[0].loadedRows,1);assert.equal(hidden.tables[0].rows[0][0],'Current result');
    assert.equal(hidden.tables[0].totalRows,null);
    assert.equal((await fixture('<table aria-rowcount="0" style="width:100px;height:10px"></table>')).tables[0].totalRows,0);
    await evaluate('Object.defineProperty(globalThis,"__novaWebview",{value:{apiVersion:7},configurable:true}); true');
    await evaluate(engine);
    assert.equal(await evaluate('__novaWebview.apiVersion'),12,'Existing Chrome worlds must upgrade to control-level DOM hints');
    assert.equal((await call('observe',randomUUID())).tables[0].totalRows,0);
  });
  await run('native and ARIA checkboxes expose role and current selection',async()=>{
    const obs=await fixture('<label><input type="checkbox">United States</label><label><input type="radio" checked>收入</label><div role="checkbox" aria-label="仅已发布" aria-checked="mixed" tabindex="0">仅已发布</div>');
    assert.equal(target(obs,'United States').role,'checkbox');
    assert.equal(target(obs,'United States').selected,false);
    assert.equal(target(obs,'收入').role,'radio');
    assert.equal(target(obs,'收入').selected,true);
    assert.equal(target(obs,'仅已发布').selected,'mixed');
    assert.equal(target(obs,'仅已发布').tabIndex,0);
    await click(target(obs,'United States'));
    assert.equal(target(await call('observe',randomUUID()),'United States').selected,true);
  });
  await run('accessible labels: aria-labelledby, multi-label fields, shadow roots',async()=>{
    const obs=await fixture('<span id="a">保存</span><span id="b">草稿</span><button aria-labelledby="a b" onclick="window.saved=event.isTrusted"></button><label for="field">客户</label><label for="field">地址</label><input id="field"><div id="host"></div>');
    assert.ok(target(obs,'客户 地址').editable);
    await click(target(obs,'保存 草稿'));assert.equal(await page.evaluate(()=>window.saved),true);
    await page.evaluate(()=>{const shadow=document.querySelector('#host').attachShadow({mode:'open'});shadow.innerHTML='<span id="s">影子按钮</span><button aria-labelledby="s"><span>icon</span></button>';shadow.querySelector('button').onclick=()=>window.shadowClicked=true;});
    const fresh=await call('observe',randomUUID());await click(target(fresh,'影子按钮'));assert.equal(await page.evaluate(()=>window.shadowClicked),true);
  });
  await run('virtualized recycled node cannot change row identity unnoticed',async()=>{
    const obs=await fixture('<table><tr><td id="row">客户甲</td><td><button>删除</button></td></tr></table>');
    await page.evaluate(()=>document.querySelector('#row').textContent='客户乙');
    await assert.rejects(call('prepare',target(obs,'删除').ref),/引用已失效/);
  });
  await run('detached / replaced same-name node is never silently retargeted',async()=>{
    const obs=await fixture('<button id="old">确定</button>');
    await page.evaluate(()=>document.querySelector('button').outerHTML='<button id="new">确定</button>');
    await assert.rejects(call('prepare',target(obs,'确定').ref),/引用已失效/);
  });
  await run('fieldset disabled, inherited aria-disabled and readonly are refused',async()=>{
    const obs=await fixture('<fieldset disabled><button>禁用</button></fieldset><div aria-disabled="true"><button>继承禁用</button></div><input aria-label="只读" readonly>');
    assert.equal(target(obs,'禁用').disabled,true);assert.equal(target(obs,'继承禁用').disabled,true);
    assert.equal(target(obs,'只读').editable,false);
    await assert.rejects(call('prepare',target(obs,'只读').ref,'fill'),/不可填写|可填写|只读/);
  });
  await run('partial occlusion chooses an exposed point; complete occlusion refuses',async()=>{
    const obs=await fixture('<button id="button" style="position:absolute;left:100px;top:100px;width:200px;height:60px" onclick="window.hit=event.isTrusted">点这里</button><div style="position:absolute;left:175px;top:100px;width:50px;height:60px;background:red;z-index:9"></div>');
    await click(target(obs,'点这里'));assert.equal(await page.evaluate(()=>window.hit),true);
    await page.evaluate(()=>document.body.insertAdjacentHTML('beforeend','<div style="position:fixed;inset:0;background:gray;z-index:99"></div>'));
    await assert.rejects(call('prepare',target(obs,'点这里').ref),/遮挡/);
  });
  await run('moving controls wait for geometry stability, without a fixed 600ms sleep',async()=>{
    const obs=await fixture('<button id="moving" style="position:absolute;top:70px;left:10px">移动目标</button>');
    await page.evaluate(()=> {window.movingDone=false;document.querySelector('button').animate([{transform:'translateX(0px)'},{transform:'translateX(260px)'}],{duration:260,fill:'forwards'}).finished.then(()=>window.movingDone=true);});
    const start=performance.now();const p=await call('prepare',target(obs,'移动目标').ref);
    assert.equal(await page.evaluate(()=>window.movingDone),true);
    assert.ok(p.x>260);assert.ok(performance.now()-start<1200);
  });
  await run('hover replacement / overlay is rechecked before mouse down',async()=>{
    const obs=await fixture('<button style="position:absolute;left:200px;top:300px" onmouseenter="this.style.transform=\'translateX(150px)\'">悬停变化</button>');
    const item=target(obs,'悬停变化'),p=await call('prepare',item.ref);
    await cdp.send('Input.dispatchMouseEvent',{type:'mouseMoved',x:p.x,y:p.y});
    await assert.rejects(call('verifyPoint',item.ref,p.x,p.y,p.rect),/改变|遮挡/);
  });
  await run('coordinate dropdown hover permits highlighting but refuses replacement, movement and overlays',async()=>{
    for(const mutation of ['highlight','replace','move','overlay','rename','disabled']) {
      const obs=await fixture('<style>#option{position:absolute;left:150px;top:180px;width:240px;height:32px}#option:hover{background:rgb(40,150,240)}</style><div id="option">Leave-Pay</div>');
      await cdp.send('Input.dispatchMouseEvent',{type:'mouseMoved',x:10,y:10});
      const p=await call('coordinate',220,196,obs.stamp,false);
      assert.ok(p.hoverTarget,'unlabelled custom options still have a live hit target');
      await page.evaluate(mutation=>{
        const e=document.querySelector('#option');
        e.onmouseenter=()=>{
          if(mutation==='replace')e.outerHTML=e.outerHTML;
          if(mutation==='move')e.style.left='450px';
          if(mutation==='overlay')document.body.insertAdjacentHTML('beforeend','<div style="position:fixed;inset:0;z-index:99"></div>');
          if(mutation==='rename')e.textContent='Delete';
          if(mutation==='disabled')e.setAttribute('aria-disabled','true');
        };
        e.onclick=()=>window.optionClicked=true;
      },mutation);
      await cdp.send('Input.dispatchMouseEvent',{type:'mouseMoved',x:p.x,y:p.y});
      const verify=()=>call('verifyPoint',p.hoverTarget.ref,p.x,p.y,p.hoverTarget.rect);
      if(mutation==='highlight') {
        await verify();
        assert.equal(await page.locator('#option').evaluate(e=>getComputedStyle(e).backgroundColor),'rgb(40, 150, 240)');
        for(const type of ['mousePressed','mouseReleased'])await cdp.send('Input.dispatchMouseEvent',{type,x:p.x,y:p.y,button:'left',clickCount:1});
        assert.equal(await page.evaluate(()=>window.optionClicked),true);
      } else await assert.rejects(verify(),/失效|改变|遮挡/);
    }
    const link=await fixture('<a href="/one" style="display:block;width:300px;height:40px"><span>链接</span></a>');
    const hit=await call('coordinate',10,10,link.stamp,false);
    await page.evaluate(()=>document.querySelector('a').setAttribute('href','/two'));
    await assert.rejects(call('verifyPoint',hit.hoverTarget.ref,hit.x,hit.y,hit.hoverTarget.rect),/失效/);
    for(const markup of ['<canvas width="500" height="400"></canvas>','<svg width="500" height="400"><rect width="500" height="400"/></svg>']) {
      const obs=await fixture(markup);
      assert.equal((await call('coordinate',100,100,obs.stamp,false)).hoverTarget,undefined,'visual surfaces retain pixel validation');
    }
  });
  await run('fill focus checks catch application redirection, including shadow inputs',async()=>{
    const obs=await fixture('<input aria-label="来源" onclick="document.querySelector(\'#other\').focus()"><input id="other" aria-label="禁止覆盖" value="保留">');
    const item=target(obs,'来源');await click(item);
    assert.equal((await call('inputState',item.ref)).matches,false);
    assert.equal(await page.locator('#other').inputValue(),'保留');
  });
  await run('offscreen nested scroll target is reached without clicking its previous location',async()=>{
    const obs=await fixture('<div style="height:160px;width:300px;overflow:auto"><div style="height:550px"></div><button onclick="window.scrolledHit=true">内部尾部</button></div>');
    await click(target(obs,'内部尾部'));assert.equal(await page.evaluate(()=>window.scrolledHit),true);
  });
  await run('Canvas metadata, scaled display and trusted pixel-coordinate pointer events',async()=>{
    const obs=await fixture('<canvas id="canvas" width="1400" height="1000" aria-label="画布" style="width:700px;height:500px;transform:scale(.9);transform-origin:0 0"></canvas>');
    assert.equal(obs.canvases.length,1);assert.equal(obs.visualSuggested,true);
    assert.equal(obs.canvases[0].backingWidth,1400);assert.equal(target(obs,'画布').canvas.content.startsWith('visual-only'),true);
    await page.evaluate(()=>{
      const canvas=document.querySelector('canvas'),c=canvas.getContext('2d');c.fillStyle='black';c.fillRect(0,0,1400,1000);c.fillStyle='lime';c.fillRect(400,200,120,100);
      canvas.addEventListener('pointerdown',e=>window.canvasHit={trusted:e.isTrusted,x:e.offsetX,y:e.offsetY});
    });
    const image=await cdp.send('Page.captureScreenshot',{format:'png',captureBeyondViewport:false,clip:{x:0,y:0,width:630,height:450,scale:1}});
    const bytes=Buffer.from(image.data,'base64');const pixelWidth=bytes.readUInt32BE(16),pixelHeight=bytes.readUInt32BE(20);
    assert.ok(pixelWidth>0 && pixelHeight>0);assert.equal(obs.viewport.devicePixelRatio,2);
    // A point selected in the returned image maps to CSS without using canvas.width or DPR as coordinates.
    const pixelX=207*pixelWidth/630,pixelY=112.5*pixelHeight/450,x=pixelX*630/pixelWidth,y=pixelY*450/pixelHeight;
    const p=await call('coordinate',x,y,obs.stamp,false);assert.equal(p.canvas,true);
    await cdp.send('Input.dispatchMouseEvent',{type:'mousePressed',x:p.x,y:p.y,button:'left',clickCount:1});
    await cdp.send('Input.dispatchMouseEvent',{type:'mouseReleased',x:p.x,y:p.y,button:'left',clickCount:1});
    const hit=await page.evaluate(()=>window.canvasHit);assert.equal(hit.trusted,true);assert.ok(Math.abs(hit.x-230)<1);assert.ok(Math.abs(hit.y-125)<1);
    if(process.env.TEST_SCREENSHOT)await page.screenshot({path:process.env.TEST_SCREENSHOT});
  });
  await run('isolated reference store survives page tampering; reinjection is idempotent',async()=>{
    const obs=await fixture('<button>隔离目标</button>');
    await page.evaluate(()=>window.__novaWebview={prepare:()=>({x:1,y:1})});
    await evaluate(engine);await call('prepare',target(obs,'隔离目标').ref);
    await call('observe',randomUUID());await assert.rejects(call('prepare',target(obs,'隔离目标').ref),/失效/);
  });
  await run('readonly inheritance and exact post-fill value verification',async()=>{
    const obs=await fixture('<input aria-label="填写" maxlength="3"><div aria-readonly="true"><div contenteditable="true" aria-label="只读区域"></div></div>');
    assert.equal(target(obs,'只读区域').editable,false);
    const item=target(obs,'填写');await click(item);
    await cdp.send('Input.insertText',{text:'abcd'});
    assert.equal((await call('verifyValue',item.ref,'abcd')).matches,false);
    assert.equal((await call('verifyValue',item.ref,'abc')).matches,true);
  });
  await run('viewport fast path explicitly excludes offscreen references',async()=>{
    await fixture('<button>可见目标</button><div style="height:1200px"></div><button>屏外目标</button>');
    const fast=await call('observe',randomUUID(),20000,'viewport');
    assert.equal(fast.items.length,1);assert.ok(fast.coverage.scope.includes('viewport DOM only'));
    assert.ok(target(await call('observe',randomUUID(),20000,'all'),'屏外目标'));
  });
  await run('stale screenshot viewport refuses after scrolling',async()=>{
    const obs=await fixture('<button>页头</button><div style="height:2000px"></div>');
    await page.evaluate(()=>scrollTo(0,450));
    await assert.rejects(call('coordinate',10,10,obs.stamp,false),/视口已变化/);
    const fresh=await call('observe',randomUUID());
    const p=await call('coordinate',10,10,fresh.stamp,false);assert.equal(p.pageY,460);
    await assert.rejects(call('coordinate',901,10,fresh.stamp,false),/超出视口/);
    await page.evaluate(()=>scrollTo(0,0));
  });
  await run('production iframe transform mapping handles scale and refuses overlay / rotation',async()=>{
    await fixture('<iframe style="width:400px;height:300px;border:4px solid black;transform:scale(.75);transform-origin:0 0" srcdoc="<button style=margin:40px onclick=window.clicked=event.isTrusted>Frame</button>"></iframe>');
    await page.frames()[1].waitForSelector('button');
    const native=await readFile(new URL('../src-tauri/src/native_browser.rs',import.meta.url),'utf8');
    const functions=[...native.matchAll(/"functionDeclaration":("(?:[^"\\]|\\.)*")/g)].map(match=>JSON.parse(match[1]));
    const mapping=functions.find(f=>f.startsWith('function(p){const r=this.getBoundingClientRect()'));
    assert.ok(mapping,'test must use the actual Rust-embedded mapping script');
    const tree=await cdp.send('Page.getFrameTree'),frame=tree.frameTree.childFrames[0].frame;
    const childWorld=(await cdp.send('Page.createIsolatedWorld',{frameId:frame.id,worldName:'nova-child-precision-test'})).executionContextId;
    const childEval=async expression=>{const r=await cdp.send('Runtime.evaluate',{contextId:childWorld,expression,returnByValue:true,awaitPromise:true});if(r.exceptionDetails)throw Error(r.exceptionDetails.text);return r.result.value;};
    await childEval(engine);const obs=await childEval('__novaWebview.observe("child")');
    const p=await childEval(`__novaWebview.prepare(${JSON.stringify(obs.items[0].ref)})`);
    const owner=await cdp.send('DOM.getFrameOwner',{frameId:frame.id});
    const resolved=await cdp.send('DOM.resolveNode',{backendNodeId:owner.backendNodeId,executionContextId:world});
    const objectId=resolved.object.objectId;
    const project=()=>cdp.send('Runtime.callFunctionOn',{objectId,functionDeclaration:mapping,arguments:[{value:p}],returnByValue:true});
    let mapped=await project();assert.equal(mapped.exceptionDetails,undefined);
    const point=mapped.result.value;
    for(const type of ['mousePressed','mouseReleased'])await cdp.send('Input.dispatchMouseEvent',{type,x:point.x,y:point.y,button:'left',clickCount:1});
    assert.equal(await page.frames()[1].evaluate(()=>window.clicked),true);
    await page.evaluate(()=>document.body.insertAdjacentHTML('beforeend','<div id="overlay" style="position:fixed;inset:0;background:black;z-index:9999"></div>'));
    assert.ok((await project()).exceptionDetails);
    await page.evaluate(()=>{document.querySelector('#overlay').remove();document.querySelector('iframe').style.transform='rotate(12deg)';});
    assert.ok((await project()).exceptionDetails);
    await cdp.send('Runtime.releaseObject',{objectId});
  });
  await run('Canvas crop offsets, multiple output scales and drag/wheel stay in CSS coordinates',async()=>{
    const obs=await fixture('<canvas width="1200" height="800" style="position:absolute;left:100px;top:120px;width:600px;height:400px"></canvas>');
    await page.evaluate(()=>{const canvas=document.querySelector('canvas');canvas.onpointerdown=e=>{window.down={x:e.offsetX,y:e.offsetY,trusted:e.isTrusted};};canvas.onpointermove=e=>{if(e.buttons===1)(window.moves??=[]).push({x:e.offsetX,y:e.offsetY});};canvas.onpointerup=e=>window.up={x:e.offsetX,y:e.offsetY};canvas.onwheel=e=>window.wheel={x:e.deltaX,y:e.deltaY,trusted:e.isTrusted};});
    for(const scale of [.5,1,2]) {
      const clip={x:150,y:180,width:200,height:160,scale};
      const image=await cdp.send('Page.captureScreenshot',{format:'png',captureBeyondViewport:false,clip});
      const b=Buffer.from(image.data,'base64'),w=b.readUInt32BE(16),h=b.readUInt32BE(20);
      const p=await call('coordinate',clip.x+w*.4*clip.width/w,clip.y+h*.5*clip.height/h,obs.stamp,false);
      for(const type of ['mousePressed','mouseReleased'])await cdp.send('Input.dispatchMouseEvent',{type,x:p.x,y:p.y,button:'left',clickCount:1});
      const hit=await page.evaluate(()=>window.down);assert.equal(hit.trusted,true);assert.equal(hit.x,130);assert.equal(hit.y,140);
    }
    await cdp.send('Input.dispatchMouseEvent',{type:'mousePressed',x:230,y:260,button:'left',buttons:1,clickCount:1});
    for(let i=1;i<=12;i++)await cdp.send('Input.dispatchMouseEvent',{type:'mouseMoved',x:230+i*5,y:260+i*3,button:'left',buttons:1});
    await cdp.send('Input.dispatchMouseEvent',{type:'mouseReleased',x:290,y:296,button:'left',clickCount:1});
    assert.ok((await page.evaluate(()=>window.moves)).length>=12);assert.deepEqual(await page.evaluate(()=>window.up),{x:190,y:176});
    await cdp.send('Input.dispatchMouseEvent',{type:'mouseWheel',x:290,y:296,deltaX:75,deltaY:120});
    await page.waitForFunction(()=>window.wheel?.trusted);assert.equal((await page.evaluate(()=>window.wheel)).x,75);
  });
  await run('a form status update does not invalidate another identified field',async()=>{
    const obs=await fixture('<form id="customer"><h2>客户资料</h2><input id="first" aria-label="第一项"><input id="second" aria-label="第二项"><span id="status">未保存</span></form>');
    await page.evaluate(()=>document.querySelector('#status').textContent='第一项已保存');
    await call('prepare',target(obs,'第二项').ref,'fill');
    await page.evaluate(()=>document.querySelector('h2').textContent='另一个客户资料');
    await assert.rejects(call('prepare',target(obs,'第二项').ref,'fill'),/失效/);
  });
  await run('semantic cache invalidates synchronously for label / row mutations',async()=>{
    const obs=await fixture('<section data-row-key="A"><span id="label">客户甲</span><button aria-labelledby="label">图标</button></section>');
    assert.equal(obs.items[0].name,'客户甲');
    const next=await evaluate(`(()=>{document.querySelector('#label').textContent='客户乙';document.querySelector('section').dataset.rowKey='B';return __novaWebview.observe('after-sync-mutation')})()`);
    assert.equal(next.items[0].name,'客户乙');assert.ok(next.items[0].region.includes('B'));
    await call('prepare',next.items[0].ref);
    const value=await fixture('<input aria-label="字段" value="old">');
    await call('observe','warm');await page.evaluate(()=>document.querySelector('input').value='new');
    assert.equal((await call('observe','fresh')).items[0].value,'new','values must not come from the semantic cache');
  });
  await run('WebGL Canvas remains observable without reading its drawing implementation',async()=>{
    const obs=await fixture('<canvas aria-label="WebGL" width="500" height="400" style="width:500px;height:400px"></canvas>');
    const supported=await page.evaluate(()=>{const canvas=document.querySelector('canvas'),gl=canvas.getContext('webgl',{preserveDrawingBuffer:true});if(!gl)return false;gl.clearColor(.1,.8,.2,1);gl.clear(gl.COLOR_BUFFER_BIT);canvas.onpointerdown=e=>window.glHit=e.isTrusted;return true;});
    if(!supported)throw Object.assign(new Error('This runner cannot create a WebGL context; rerun with REQUIRE_WEBGL=1 on a graphics-capable machine.'),{code:'UNSUPPORTED_WEBGL'});
    assert.equal(obs.visualSuggested,true);assert.equal(obs.canvases[0].backingWidth,500);
    await click(target(obs,'WebGL'));assert.equal(await page.evaluate(()=>window.glHit),true);
    const image=await cdp.send('Page.captureScreenshot',{format:'png',captureBeyondViewport:false});assert.ok(Buffer.from(image.data,'base64').length>200);
  });
  // Compare collection latency against the exact supplied baseline on identical loaded DOM.
  const benchmark = async (source,scope='all')=>{
    await fixture('<div id="fixture"></div>');
    await page.evaluate(()=>{document.querySelector('#fixture').innerHTML=Array.from({length:1000},(_,i)=>`<section><label for="i${i}">字段${i}</label><input id="i${i}"><button>操作${i}</button></section>`).join('');});
    await evaluate('delete globalThis.__novaWebview;'+source);
    const samples=[];let coldMs;for(let i=0;i<9;i++){const time=await evaluate(`(()=>{const t=performance.now();__novaWebview.observe(${JSON.stringify(randomUUID())},20000,${JSON.stringify(scope)});return performance.now()-t})()`);if(i===0)coldMs=time;if(i>=2)samples.push(time);}
    samples.sort((a,b)=>a-b);const itemCount=await evaluate(`__novaWebview.observe('benchmark-count',20000,${JSON.stringify(scope)}).items.length`);
    return {scope,itemCount,coldMs,medianMs:samples[Math.floor(samples.length/2)],samplesMs:samples};
  };
  if (process.env.BASELINE_PAGE_SCRIPT) report.benchmarks.before=await benchmark(await readFile(process.env.BASELINE_PAGE_SCRIPT,'utf8'));
  report.benchmarks.after=await benchmark(engine);
  report.benchmarks.viewport=await benchmark(engine,'viewport');
  console.log(JSON.stringify(report.benchmarks,null,2));
} catch(error) {report.error=String(error.stack||error);throw error;}
finally {await writeFile(process.env.TEST_REPORT || 'browser-precision-report.json',JSON.stringify(report,null,2));await browser?.close();}
