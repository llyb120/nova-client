import assert from 'node:assert/strict';
import { test } from 'node:test';
import { performance } from 'node:perf_hooks';
import { pageThread, mergeHistoryPage, mergeHistoryUpdate, preserveOptimistic, windowBytes, HISTORY_WINDOW_BYTES } from '../src/historyWindow.ts';
import { buildTimelineGraph, visibleTimeline } from '../src/timelineGraph.ts';

const item = (i, text = `message ${i}`) => ({ type: i % 3 === 0 ? 'user' : 'assistant', id:i+1, historyIndex:i, text, ts:i });
const stats = (n) => ({ items:n, users:Math.ceil(n/3), turns:0, estimatedBytes:n*120, inlineAssetBytes:0, totalTokens:1234567, inputTokens:100,outputTokens:50,cacheReadTokens:10,cacheWriteTokens:0,maxId:n });
const page = (start,end,total=1000,generation='g') => ({thread:{id:'a',title:'history',cwd:'/test',createdAt:0,updatedAt:0,items:Array.from({length:end-start},(_,i)=>item(start+i))},generation,start,end,totalItems:total,turnOffset:Math.ceil(start/3),beforeCursor:start?`${generation}:${start}`:null,afterCursor:end<total?`${generation}:${end}`:null,stats:stats(total)});

test('paged history remains bounded through both directions; every index is contiguous and counts stay global',()=>{
 let t=pageThread(page(920,1000));
 for(let end=920;end>0;end-=80){t=mergeHistoryPage(t,page(Math.max(0,end-80),end),'before');assert.ok(t.items.length<=240);assert.ok(windowBytes(t.items)<=HISTORY_WINDOW_BYTES);}
 assert.equal(t.history.start,0);assert.equal(t.history.beforeCursor,null);assert.equal(t.history.stats.totalTokens,1234567);
 while(t.history.afterCursor){const start=t.history.end;t=mergeHistoryPage(t,page(start,Math.min(start+80,1000)),'after');assert.ok(t.items.length<=240);assert.ok(t.items.every((v,i,a)=>!i||v.historyIndex===a[i-1].historyIndex+1));}
 assert.equal(t.history.end,1000);assert.equal(t.history.turnOffset,Math.ceil(t.history.start/3));
});
test('overlapping old page never overwrites newer authoritative streamed text',()=>{
 let t=pageThread(page(80,160,160));t.items[0].text='new stream';
 t=mergeHistoryPage(t,page(40,100,160),'before');assert.equal(t.items.find(i=>i.id===81).text,'new stream');assert.equal(new Set(t.items.map(i=>i.id)).size,t.items.length);
});
test('generation mismatch and gaps reject instead of silently splicing unrelated histories',()=>{
 const t=pageThread(page(920,1000));assert.throws(()=>mergeHistoryPage(t,page(0,80),'before'),/HISTORY_GAP/);
 assert.throws(()=>mergeHistoryUpdate(t,{generation:'restored',items:[],totalItems:1000,stats:stats(1000)}),/HISTORY_CHANGED/);
 assert.throws(()=>mergeHistoryPage(t,page(840,920,1000,'restored'),'before'),/HISTORY_CHANGED/);
});
test('a canonical update is replacement, duplicate invalidation cannot repeat deltas',()=>{
 let t=pageThread(page(920,1000));const update={generation:'g',items:[item(999,'answer complete')],totalItems:1000,stats:stats(1000)};
 t=mergeHistoryUpdate(t,update).thread;t=mergeHistoryUpdate(t,update).thread;assert.equal(t.items.at(-1).text,'answer complete');assert.equal(t.items.length,80);
 const gap=mergeHistoryUpdate(t,{...update,items:[item(1002)],totalItems:1003,stats:stats(1003)});assert.equal(gap.gap,true);assert.equal(gap.thread.items.length,80);
});
test('background output updates the count without dragging an old reading window to the tail',()=>{
 const t=pageThread(page(40,120));const {thread,gap}=mergeHistoryUpdate(t,{generation:'g',items:[item(1000,'new')],totalItems:1001,stats:stats(1001)});
 assert.equal(gap,false);assert.deepEqual(thread.items,t.items);assert.equal(thread.history.totalItems,1001);assert.equal(thread.history.afterCursor,'g:120');
});
test('byte budget trims much earlier than item count for large previews; source remains untouched',()=>{
 const p=page(920,1000);p.thread.items=p.thread.items.map(i=>({...i,text:'多'.repeat(24000)}));const source=pageThread(p);
 const next=mergeHistoryUpdate(source,{generation:'g',items:[],totalItems:1000,stats:stats(1000)}).thread;
 assert.ok(windowBytes(next.items)<=HISTORY_WINDOW_BYTES);assert.ok(next.items.length<30);assert.equal(source.items.length,80);assert.equal(next.items.at(-1).id,1000);
});
test('scrolling into old users is not acknowledgement of an optimistic send',()=>{
 const t=pageThread(page(100,180));t.items.push({...item(0,'optimistic'),id:-10,historyIndex:undefined});
 const next=preserveOptimistic(t,pageThread(page(920,1000)));assert.equal(next.items.at(-1).id,-10);
 const replaced=mergeHistoryUpdate(t,{generation:'g',items:[item(102)],totalItems:1000,stats:stats(1000)}).thread;assert.equal(replaced.items.at(-1).id,-10);
 const acknowledged=preserveOptimistic(t,pageThread(page(923,1003,1003)));assert.equal(acknowledged.items.some(i=>i.id<0),false);
});
test('10,000 timeline prompts use bounded IDs, iterative layout and a viewport-sized DOM model',()=>{
 const prompts=Array.from({length:10000},(_,id)=>({id,text:'记录 '+id+' 中'.repeat(100)}));
 const checkpoint={id:'c',prompts:prompts.slice(0,7000),title:'checkpoint',createdAt:0};
 const began=performance.now();const graph=buildTimelineGraph([checkpoint],prompts);const elapsedMs=performance.now()-began;
 assert.equal(graph.nodes.length,10000);assert.ok(graph.nodes.every(n=>n.id.length<40));assert.equal(new Set(graph.nodes.map(n=>n.id)).size,10000);
 assert.equal(graph.nodes.at(-1).current,true);assert.equal(graph.nodes[6999].checkpoint.id,'c');assert.ok(visibleTimeline(graph,120000,800).nodes.length<50);
 assert.equal(graph.edges.length,9999);console.log(JSON.stringify({test:'timeline-10000',elapsedMs,nodes:graph.nodes.length,maxIdLength:Math.max(...graph.nodes.map(n=>n.id.length))}));
});
test('same prompt id with edited local text is a distinct branch; exact paths still coalesce',()=>{
 const current=[{id:1,text:'start'},{id:2,text:'new'}];const graph=buildTimelineGraph([{id:'old',prompts:[current[0],{id:2,text:'old'}]}],current);
 assert.equal(graph.nodes.length,3);assert.equal(graph.nodes.filter(n=>n.onCurrentPath).length,2);assert.equal(graph.laneCount,2);assert.ok(graph.nodes.find(n=>n.title==='old').previewCheckpoint);
});


test('an in-flight prepend cannot evict a viewport after the reader reverses direction',()=>{
 const current=pageThread(page(400,640));
 const next=mergeHistoryPage(current,page(320,400),'before',[621,640]);
 assert.ok(next.items.length<=240);assert.ok(windowBytes(next.items)<=HISTORY_WINDOW_BYTES);
 for(let id=621;id<=640;id++)assert.ok(next.items.some(i=>i.id===id));
 assert.ok(next.items.every((v,i,a)=>!i||v.historyIndex===a[i-1].historyIndex+1));
 assert.equal(next.history.turnOffset,Math.ceil(next.history.start/3));
});
test('an in-flight append cannot evict the viewport at the older edge',()=>{
 const current=pageThread(page(400,640));
 const next=mergeHistoryPage(current,page(640,720),'after',[401,420]);
 assert.ok(next.items.length<=240);assert.ok(windowBytes(next.items)<=HISTORY_WINDOW_BYTES);
 for(let id=401;id<=420;id++)assert.ok(next.items.some(i=>i.id===id));
 assert.ok(next.items.every((v,i,a)=>!i||v.historyIndex===a[i-1].historyIndex+1));
});
