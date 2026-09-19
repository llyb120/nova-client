import assert from 'node:assert/strict';
import { readFile, writeFile, mkdir } from 'node:fs/promises';
import { browserFixture } from './lib/browser-fixture.mjs';
const fixture=await readFile('scripts/fixtures/history-ui-fixture.tsx','utf8');
// The reusable fixture is compiled as a temporary root entry.
const source=fixture.replaceAll("'../../src/","'./src/");
const results=[];
const {page,errors,close}=await browserFixture(source,'long-image-history');
const reportDir=process.env.TEST_REPORT_DIR||'history-ui-results';await mkdir(reportDir,{recursive:true});
async function check(name,run){const start=Date.now();await run();results.push({name,result:'passed',elapsedMs:Date.now()-start});console.log('PASS '+name);}
try {
 await page.waitForFunction(()=>!!window.perfTest);
 await check('10,000 image-heavy rounds load only a bounded page, not full history or hidden timeline',async()=>{
  await page.evaluate(()=>window.perfTest.open('big'));
  await page.waitForFunction(()=>window.perfTest.metrics().paints>10 && !window.perfTest.state.loadingThread);
  await page.waitForTimeout(250);
  const metrics=await page.evaluate(()=>window.perfTest.metrics());
  assert.ok(metrics.items<=80);assert.ok(metrics.bytes<1536*1024);assert.ok(metrics.firstPaint<1500);
  assert.equal(await page.evaluate(()=>window.perfTest.calls.filter(c=>c.command==='get_thread').length),0);
  assert.equal(await page.evaluate(()=>window.perfTest.calls.filter(c=>c.command==='get_time_machine_timeline'||c.command==='get_thread_outline').length),0);
  assert.equal(await page.locator('.repo-time-node').count(),0);assert.ok(metrics.images<40);assert.ok(metrics.imageMax<=2);
  results.push({name:'cold-open-metrics',...metrics});
 });
 await check('pagination keeps a contiguous 240-item/1.5MiB window and does not request originals',async()=>{
  for(let i=0;i<5;i++){await page.evaluate(()=>{void window.perfTest.page('before');});await page.waitForFunction(()=>!window.perfTest.state.historyLoading);}
  const data=await page.evaluate(()=>({ids:window.perfTest.state.items.map(i=>i.historyIndex),h:window.perfTest.metadata(),m:window.perfTest.metrics()}));
  assert.ok(data.m.items<=240);assert.ok(data.m.bytes<1536*1024);assert.ok(data.ids.every((id,i,a)=>!i||id===a[i-1]+1));assert.ok(data.h.beforeCursor&&data.h.afterCursor);
  assert.equal(await page.evaluate(()=>window.perfTest.images.filter(i=>i.original).length),0);
 });
 await check('stop reaches native IPC even while a historical page is outstanding; acknowledgement is not fake completion',async()=>{
  await page.evaluate(()=>{window.perfTest.run(true);window.perfTest.gate();});
  await page.evaluate(()=>{void window.perfTest.page('before');});
  await page.waitForFunction(()=>window.perfTest.state.historyLoading);
  await page.getByRole('button',{name:'停止会话',exact:true}).click();
  await page.waitForFunction(()=>window.perfTest.calls.some(c=>c.command==='cancel_turn'));
  assert.equal(await page.getByRole('button',{name:'正在停止',exact:true}).isDisabled(),true);
  assert.equal(await page.evaluate(()=>window.perfTest.state.historyLoading),true);
  await page.evaluate(()=>window.perfTest.cancelAck());await page.waitForFunction(()=>!window.perfTest.state.stopping.big);
  assert.equal(await page.evaluate(()=>window.perfTest.state.running.big),true);
  await page.evaluate(()=>window.perfTest.release());await page.waitForFunction(()=>!window.perfTest.state.historyLoading);
  await page.evaluate(()=>window.perfTest.run(false));
 });
 await check('send stays responsive from an old page and canonical feed removes the optimistic duplicate',async()=>{
  await page.locator('textarea.composer-input').fill('a new message after 10000 rounds');
  await page.locator('.composer-btn.send').click();
  await page.waitForFunction(()=>window.perfTest.calls.some(c=>c.command==='send_prompt'));
  await page.waitForFunction(()=>window.perfTest.state.items.some(i=>i.id>0&&i.text==='a new message after 10000 rounds'));
  assert.equal(await page.evaluate(()=>window.perfTest.state.items.filter(i=>i.text==='a new message after 10000 rounds').length),1);
  assert.equal(await page.evaluate(()=>window.perfTest.state.items.some(i=>i.id<0)),false);assert.ok(await page.evaluate(()=>window.perfTest.state.items.length<=240));
  await page.evaluate(()=>window.perfTest.run(false));
 });
 await check('authoritative invalidations update, never double-append text, and retain an older reading range',async()=>{
  await page.evaluate(()=>window.perfTest.page('before'));const before=await page.evaluate(()=>window.perfTest.metadata());
  const target=await page.evaluate(()=>window.perfTest.state.items.find(i=>i.type==='assistant').id);
  await page.evaluate(id=>{window.perfTest.update(id,'authoritative new text');for(let n=0;n<30;n++)window.perfTest.notice([id]);},target);
  await page.waitForFunction(id=>window.perfTest.state.items.find(i=>i.id===id)?.text==='authoritative new text',target);
  await page.evaluate(()=>window.perfTest.notice([],true));await page.waitForTimeout(300);
  const after=await page.evaluate(()=>window.perfTest.metadata());assert.equal(after.start,before.start);assert.equal(after.end,before.end);
 });
 await check('collapsed worldline is lazy; opening 10,000 prompts mounts only visible nodes',async()=>{
  await page.getByRole('button',{name:'展开世界线',exact:true}).click();
  await page.waitForFunction(()=>window.perfTest.calls.some(c=>c.command==='get_thread_outline'));
  await page.waitForFunction(()=>document.querySelectorAll('.repo-time-node').length>0);
  assert.ok(await page.locator('.repo-time-node').count()<60);
  await page.locator('header').getByRole('button',{name:'收起世界线',exact:true}).click();assert.equal(await page.locator('.repo-time-node').count(),0);
 });
 await check('late page replies cannot overwrite a newer session; failed page does not lose the current window',async()=>{
  await page.evaluate(()=>{window.perfTest.defer('slow');void window.perfTest.open('slow');});await page.waitForFunction(()=>window.perfTest.state.currentId==='slow');
  await page.evaluate(()=>window.perfTest.open('small'));await page.evaluate(()=>window.perfTest.release());await page.waitForTimeout(120);
  assert.equal(await page.evaluate(()=>window.perfTest.state.currentId),'small');assert.equal(await page.evaluate(()=>window.perfTest.state.items.length),12);
  await page.evaluate(()=>window.perfTest.open('big'));const before=await page.evaluate(()=>window.perfTest.state.items.map(i=>i.id));
  await page.evaluate(()=>window.perfTest.fail());await page.evaluate(()=>{void window.perfTest.page('before');});
  await page.getByRole('alert').getByText('fixture: disk failure',{exact:false}).waitFor();
  assert.deepEqual(await page.evaluate(()=>window.perfTest.state.items.map(i=>i.id)),before);
 });
 await check('one explicitly expanded message resolves full text without replacing the bounded history',async()=>{
  await page.evaluate(()=>window.perfTest.page('latest',161));
  // Trigger the production click target by recording its canvas draw position.
  await page.evaluate(()=>{window.detailPoint=null;const f=CanvasRenderingContext2D.prototype.fillText;CanvasRenderingContext2D.prototype.fillText=function(text,x,y,...args){if(String(text).includes('查看完整'))window.detailPoint={x,y};return f.call(this,text,x,y,...args);};});
  await page.setViewportSize({width:1250,height:900});await page.waitForFunction(()=>!!window.detailPoint);
  const point=await page.evaluate(()=>window.detailPoint),rect=await page.locator('canvas.transcript-canvas-only').boundingBox();
  await page.mouse.click(rect.x+point.x+15,rect.y+point.y-5);
  await page.getByRole('dialog',{name:'完整消息',exact:true}).waitFor();await page.waitForFunction(()=>window.perfTest.details.length>0);
  assert.ok(await page.evaluate(()=>window.perfTest.state.items.length<=240));assert.ok((await page.getByRole('dialog').locator('pre').textContent()).length<=65536);
  await page.getByRole('button',{name:'关闭完整消息',exact:true}).click();
 });
 await check('missing originals display a recoverable error instead of tearing down ChatView',async()=>{
  await page.evaluate(()=>window.dispatchEvent(new CustomEvent('nova:history-image',{detail:'nova-history://missing'})));
  await page.getByRole('dialog',{name:'原图预览'}).getByRole('alert').getByText('fixture: missing original',{exact:false}).waitFor();
  assert.equal(await page.locator('textarea.composer-input').count(),1);
  await page.getByRole('button',{name:'关闭原图预览',exact:true}).click();
 });
 await check('structural restore invalidates old cursors and safely reloads the current generation',async()=>{
  await page.evaluate(()=>window.perfTest.restore());await page.waitForFunction(()=>window.perfTest.metadata()?.generation==='g-bigr');
  assert.ok(await page.evaluate(()=>window.perfTest.state.items.every(i=>i.historyIndex<40)));assert.equal(await page.evaluate(()=>window.perfTest.state.historyError),'');
 });
 await check('individual image visibility, not a whole visible bubble, controls decoding',async()=>{
  const count=await page.evaluate(()=>window.perfTest.images.length);await page.evaluate(()=>window.perfTest.canvas('gallery'));
  await page.waitForFunction(()=>window.perfTest.canvasStats().entries>0);await page.waitForTimeout(500);
  const info=await page.evaluate(()=>({stats:window.perfTest.canvasStats(),requests:window.perfTest.images.filter(i=>i.reference.includes('/gallery/')).length}));
  const newCount=await page.evaluate(()=>window.perfTest.images.length);
  assert.ok(newCount-count<40,'Must not decode all 64 photos in the bubble: '+(newCount-count));assert.ok(info.stats.bytes<=32*1024*1024);assert.ok(info.stats.active<=2);
  results.push({name:'gallery-metrics',...info});
 });
 await check('prepend and eviction preserve the canvas message anchor',async()=>{
  await page.evaluate(()=>window.perfTest.canvas('anchor'));await page.waitForTimeout(150);
  await page.evaluate(()=>window.perfTest.canvasJump(3));await page.waitForTimeout(150);
  const anchor=await page.evaluate(()=>window.perfTest.canvasAnchor());
  await page.evaluate(()=>window.perfTest.canvasPage('before'));await page.waitForTimeout(250);
  const after=await page.evaluate(()=>window.perfTest.canvasAnchor());
  assert.equal(after.itemId,anchor.itemId);assert.ok(Math.abs(after.offset-anchor.offset)<3,JSON.stringify({anchor,after}));
 });
 assert.deepEqual(errors,[]);await page.screenshot({path:`${reportDir}/history-ui.png`});
} catch(error) {results.push({name:'failure',error:String(error.stack||error),errors});console.error(errors);console.error(await page.evaluate(()=>({body:document.body.innerText.slice(-3000),metrics:window.perfTest?.metrics(),h:window.perfTest?.metadata(),calls:window.perfTest?.calls.slice(-15)})));await page.screenshot({path:`${reportDir}/failure.png`}).catch(()=>{});throw error;}
finally {await writeFile(`${reportDir}/report.json`,JSON.stringify({mode:process.env.TEST_OFFLINE==='1'?'offline-bundled':'real-browser-http',results,errors},null,2));await close();}

// Keep the original CI entrypoint running the new real-wheel/resend regressions.
await import('./history-seamless.test.mjs');
