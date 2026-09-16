import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {runInNewContext} from 'node:vm';
import {webcrypto} from 'node:crypto';

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
    const scope={chrome,crypto:webcrypto,URL,AbortSignal,setTimeout,fetch:async url=>{discovery.push(url);throw Error('offline');}};
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
  assert.ok(discovery.every(url=>url==='http://127.0.0.1:47653/pair'));
});
