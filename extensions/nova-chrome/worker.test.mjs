import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {runInNewContext} from 'node:vm';
import {webcrypto} from 'node:crypto';

test('a stuck debugger command times out, polling resumes and late results are not replayed',async()=>{
  const event=()=>({addListener(){}});
  const replies=[]; let polls=0, calls=0, finishCommand;
  const chrome={
    storage:{session:{get:async()=>({}),set:async()=>{}}},
    tabs:{query:async()=>[{id:11,url:'https://example.com',active:true}],get:async()=>({id:11}),onRemoved:event(),onReplaced:event()},
    debugger:{attach:async()=>{},sendCommand:()=>{calls++;return new Promise(resolve=>{finishCommand=resolve;});},onDetach:event(),onEvent:event()},
    runtime:{id:'test',onMessage:event(),onInstalled:event(),onStartup:event()},alarms:{onAlarm:event()},
  };
  const scope={chrome,crypto:webcrypto,URL,AbortSignal,setTimeout,clearTimeout,fetch:async(url,options)=>{
    const endpoint=new URL(url);
    if(endpoint.port!=='47653')throw Error('offline');
    if(endpoint.pathname==='/pair')return {ok:true,json:async()=>({token:'t'.repeat(32)})};
    if(endpoint.pathname==='/reply'){replies.push(JSON.parse(options.body));return {ok:true};}
    if(polls++>=2)return new Promise(()=>{});
    const [tab]=await scope.api.inventory();
    const command=polls===1
      ? {id:'stuck',operation:'cdp',args:{tabTag:tab.tag,method:'Runtime.evaluate'},expiresAt:Date.now()+50}
      : {id:'next',operation:'tabs',args:{},expiresAt:Date.now()+3000};
    return {ok:true,json:async()=>({command})};
  }};
  const source=await readFile(new URL('./worker.js',import.meta.url),'utf8');
  runInNewContext(source+'\nglobalThis.api={inventory};',scope);
  for(let i=0;i<100 && replies.length<2;i++)await new Promise(resolve=>setTimeout(resolve,5));
  assert.equal(replies.length,2,'a hung debugger must not stop polling or block the shared queue');
  assert.match(replies[0].error,/响应超时.*不要重放/);
  assert.equal(replies[1].result.tabs.length,1);
  finishCommand({late:true});
  await new Promise(resolve=>setImmediate(resolve));
  assert.equal(calls,1);
  assert.equal(replies.length,2,'late completion cannot send another reply');
});

test('stable tags, default access, auto-attached frames, expiry and worker restart',async()=>{
  const persisted={};let nativeCalls=[];const discovery=[];
  let tabs=[{id:11,url:'https://example.com/a',title:'A',active:true},{id:22,url:'https://example.org/b',title:'B',active:false}];
  const event=()=>({listeners:[],addListener(fn){this.listeners.push(fn);},emit(...args){for(const fn of this.listeners)fn(...args);}});
  const chrome={
    storage:{session:{get:async()=>structuredClone(persisted),set:async v=>Object.assign(persisted,structuredClone(v))},local:{set:async()=>{},get:async()=>({})}},
    tabs:{query:async filter=>structuredClone(filter?.active?tabs.filter(t=>t.active):tabs),get:async id=>{const tab=tabs.find(t=>t.id===id);if(!tab)throw Error('closed');return tab;},onRemoved:event(),onReplaced:event()},
    debugger:{attach:async target=>nativeCalls.push(['attach',target.tabId]),detach:async()=>{},sendCommand:async(target,method)=>{
      nativeCalls.push([method,target.tabId]);
      if(['Target.getTargets','Target.attachToTarget'].includes(method))throw Error('{"code":-32000,"message":"Not allowed"}');
      if(method==='Target.setAutoAttach' && target.sessionId!=='child-2'){
        const n=target.sessionId?2:1;
        chrome.debugger.onEvent.emit(target,'Target.attachedToTarget',{sessionId:`child-${n}`,targetInfo:{targetId:`frame-${n}`,type:'iframe'}});
      }
      return {};
    },onDetach:event(),onEvent:event()},
    runtime:{id:'test',getURL:p=>p,onMessage:event(),onInstalled:event(),onStartup:event()},alarms:{onAlarm:event()},
  };
  const source=await readFile(new URL('./worker.js',import.meta.url),'utf8');
  const start=()=>{
    const scope={chrome,crypto:webcrypto,URL,AbortSignal,setTimeout,clearTimeout,fetch:async url=>{discovery.push(url);throw Error('offline');}};
    runInNewContext(source+'\nglobalThis.api={inventory,execute,resolveTag};',scope);return scope.api;
  };
  const api=start();const initial=await api.inventory();
  assert.notEqual(initial[0].tag,initial[1].tag);
  const command=(tag,extra={})=>({operation:'cdp',args:{tabTag:tag,method:'Input.insertText',params:{text:'test'}},expiresAt:Date.now()+3000,...extra});
  await assert.rejects(api.execute(command(undefined)),/tabTag/);
  await api.execute(command(initial[0].tag));
  // Legacy denied flags must not restrict access after upgrading.
  persisted.model.tabs[11].allowed=false;
  const restarted=start();
  tabs[0].active=false;tabs[1].active=true;tabs[0].url='https://example.com/next';
  assert.equal((await restarted.inventory())[0].tag,initial[0].tag);
  await restarted.execute(command(initial[0].tag));
  const cdp=(method,params={})=>restarted.execute({...command(initial[0].tag),args:{tabTag:initial[0].tag,method,params}});
  const frames=await cdp('Target.getTargets');
  assert.equal(frames.targetInfos.length,2);
  assert.equal((await cdp('Target.attachToTarget',{targetId:'frame-2'})).sessionId,'child-2');
  assert.ok(!nativeCalls.some(([method])=>['Target.getTargets','Target.attachToTarget'].includes(method)));
  await assert.rejects(cdp('Target.attachToTarget',{targetId:'unrelated-frame'}),/重新观察/);
  const popup=await new Promise(resolve=>chrome.runtime.onMessage.listeners.at(-1)({type:'status'},{id:'test'},resolve));
  assert.equal(popup.tab.tag,initial[1].tag);
  assert.equal('tabs' in popup,false,'Popup only exposes the active tab');
  assert.ok(nativeCalls.every(([,id])=>id===11),'Never operate on the newly active tab');
  const count=nativeCalls.length;
  await assert.rejects(restarted.execute(command(initial[0].tag,{expiresAt:0})),/过期/);
  assert.equal(nativeCalls.length,count);
  tabs=tabs.filter(t=>t.id!==11);await restarted.inventory();
  await assert.rejects(restarted.execute(command(initial[0].tag)),/失效/);
  assert.ok(discovery.length>0,'Discover Nova without a bundled config or user setup');
  assert.deepEqual([...new Set(discovery)].sort(),Array.from({length:10},(_,i)=>`http://127.0.0.1:${47653+i}/pair`));
});

test('multiple Nova instances route replies independently, serialize commands and reconnect without replay',async()=>{
  const event=()=>({addListener(){}});
  const replies=[], pairs=new Map(), polls=new Map();
  const available=new Set([47653,47654]);
  let active=0, peak=0, attaches=0;
  const chrome={
    storage:{session:{get:async()=>({}),set:async()=>{}}},
    tabs:{query:async()=>[{id:11,url:'https://example.com',active:true}],get:async()=>({id:11}),onRemoved:event(),onReplaced:event()},
    debugger:{attach:async()=>{attaches++;},sendCommand:async(_target,_method,params)=>{
      peak=Math.max(peak,++active);
      await new Promise(resolve=>setImmediate(resolve));
      active--; return {text:params.text};
    },onDetach:event(),onEvent:event()},
    runtime:{id:'test',onMessage:event(),onInstalled:event(),onStartup:event()},alarms:{onAlarm:event()},
  };
  const scope={chrome,crypto:webcrypto,URL,AbortSignal,setTimeout,clearTimeout,fetch:async(url,options)=>{
    const endpoint=new URL(url),port=Number(endpoint.port);
    if(!available.has(port))throw Error('offline');
    if(endpoint.pathname==='/pair'){
      pairs.set(port,(pairs.get(port)||0)+1);
      return {ok:true,json:async()=>({token:String(port).repeat(8),origin:'http://127.0.0.1:1'})};
    }
    assert.equal(options.headers.Authorization,`Bearer ${String(port).repeat(8)}`);
    if(endpoint.pathname==='/poll'){
      const count=polls.get(port)||0;polls.set(port,count+1);
      if(count)return new Promise(()=>{});
      const [tab]=await scope.api.inventory();
      return {ok:true,json:async()=>({command:{id:'same-id',operation:'cdp',args:{tabTag:tab.tag,method:'Input.insertText',params:{text:String(port)}},expiresAt:Date.now()+3000}})};
    }
    const reply=JSON.parse(options.body);
    replies.push({port,reply});
    if(port===47653)throw Error('reply connection lost');
    return {ok:true};
  }};
  const source=await readFile(new URL('./worker.js',import.meta.url),'utf8');
  runInNewContext(source+'\nglobalThis.api={inventory,loop,connections};',scope);
  const waitFor=async predicate=>{
    for(let i=0;i<100 && !predicate();i++)await new Promise(resolve=>setTimeout(resolve,5));
    assert.ok(predicate());
  };
  await waitFor(()=>replies.length===2 && scope.api.connections.size===1);
  for(const {port,reply} of replies){assert.equal(reply.result.text,String(port));assert.equal(reply.id,'same-id');}
  assert.equal(attaches,1,'shared tab is attached once');
  assert.equal(peak,1,'commands from separate instances cannot race a debugger attachment');
  assert.ok(scope.api.connections.has('http://127.0.0.1:47654'),'a failed instance does not disconnect the other');
  available.add(47655);
  scope.api.loop();scope.api.loop();
  await waitFor(()=>replies.length===3);
  assert.equal(pairs.get(47653),2,'failed instance is discovered again');
  assert.equal(pairs.get(47654),1,'live poll is not duplicated');
  assert.equal(pairs.get(47655),1,'new instance is discovered while another is connected');
  assert.equal(replies.filter(value=>value.port===47653).length,1,'failed delivery is never replayed');
});
