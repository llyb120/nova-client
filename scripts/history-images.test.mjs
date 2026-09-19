import assert from 'node:assert/strict';
import { test } from 'node:test';
import { build } from 'esbuild';
import { setTimeout as delay } from 'node:timers/promises';
const output=await build({entryPoints:['src/historyImages.ts'],bundle:true,write:false,platform:'node',format:'esm'});
const {TranscriptImages}=await import('data:text/javascript;base64,'+Buffer.from(output.outputFiles[0].text).toString('base64'));
const until=async predicate=>{for(let i=0;i<100&&!predicate();i++)await delay(10);assert.ok(predicate());};

test('two-slot loader skips queued offscreen pictures and releases evicted bitmaps',async()=>{
 const requests=[],pending=new Map(),disposed=[],changes=[];
 const cache=new TranscriptImages(changed=>changes.push(changed),(source)=>{requests.push(source);return new Promise(resolve=>pending.set(source,resolve));},1000,4);
 const finish=source=>pending.get(source)({drawable:{},naturalWidth:100,naturalHeight:100,bytes:400,dispose:()=>disposed.push(source)});
 cache.beginFrame();for(const source of ['a','b','off1','off2'])cache.request(source);cache.endFrame();
 await until(()=>requests.length===2);assert.equal(cache.stats().active,2);
 cache.beginFrame();cache.request('a');cache.request('b');cache.endFrame();assert.equal(cache.stats().queued,0);
 finish('a');finish('b');await until(()=>cache.stats().active===0);assert.equal(cache.stats().bytes,800);
 cache.beginFrame();cache.request('c');cache.endFrame();await until(()=>requests.includes('c'));finish('c');await until(()=>cache.stats().active===0);
 assert.ok(cache.stats().bytes<=1000);assert.equal(disposed.length,1);assert.ok(cache.dimensions('a'));assert.deepEqual(requests,['a','b','c']);
 cache.clear();assert.equal(cache.stats().bytes,0);assert.equal(disposed.length,3);
});
test('switching does not accumulate native requests or let stale completion repaint the next thread',async()=>{
 const requests=[],pending=[],disposed=[];let changed=0;
 const cache=new TranscriptImages(()=>changed++,source=>{requests.push(source);return new Promise(resolve=>pending.push(()=>resolve({drawable:{},naturalWidth:10,naturalHeight:10,bytes:400,dispose:()=>disposed.push(source)})));});
 cache.beginFrame();cache.request('old1');cache.request('old2');cache.endFrame();await until(()=>requests.length===2);
 for(let i=0;i<20;i++){cache.clear();cache.beginFrame();cache.request('new'+i);cache.endFrame();}
 await delay(20);assert.equal(requests.length,2);assert.equal(cache.stats().active,2);
 pending.shift()();pending.shift()();await until(()=>requests.length===3);assert.equal(changed,0);assert.deepEqual(disposed,['old1','old2']);assert.equal(requests[2],'new19');
 pending.shift()();await until(()=>cache.stats().active===0);assert.equal(changed,1);cache.clear();
});
test('oversize or failed pictures terminate instead of repeatedly decoding; metadata survives ordinary eviction',async()=>{
 let loads=0,closed=0;
 const cache=new TranscriptImages(()=>{},async()=>{loads++;return {drawable:{},naturalWidth:1000,naturalHeight:1000,bytes:5000,dispose:()=>closed++};},1000);
 cache.beginFrame();cache.request('large');cache.endFrame();await until(()=>cache.stats().active===0&&loads===1);
 for(let i=0;i<10;i++){cache.beginFrame();cache.request('large');cache.endFrame();}
 await delay(20);assert.equal(loads,1);assert.equal(closed,1);assert.equal(cache.stats().bytes,0);assert.match(cache.error('large'),/预算/);cache.clear();
});
