import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {test} from 'node:test';
import {transformSync} from 'esbuild';

const source = readFileSync(new URL('../src/store.ts', import.meta.url), 'utf8');
const code = transformSync(source.slice(source.indexOf('const historyLoads ='), source.indexOf('export async function openThread('))
  .replace('export async function', 'async function'), {loader:'ts'}).code;
function fixture(items, getThreadItems) {
  const state = {currentId:'one',items};
  let reopened=0, remembered=0;
  const deps = {state,api:{getThreadItems},unwrap:value=>value,batch:fn=>fn(),setTimeout,
    setState:(key,value)=>{state[key]=value;},rememberCurrentThreadSnapshot:()=>remembered++,
    staleThreadSnapshots:new Set(),openThread:async()=>reopened++};
  const runtime = new Function(...Object.keys(deps), `let openThreadRequest=1; ${code}
    return {load:ensureHistoryItems,switch:()=>{openThreadRequest++;state.currentId='two';}};`)(...Object.values(deps));
  return {...runtime,state,counts:()=>({reopened,remembered})};
}
const item = id => ({id,type:'assistant',text:'',deferred:true});
const defer = () => {let resolve,reject;const promise=new Promise((r,j)=>{resolve=r;reject=j;});return {promise,resolve,reject};};

test('viewport loading is deduplicated, immutable, and leaves unrelated history deferred',async()=>{
  const pending=defer();let calls=0;
  const f=fixture([item(1),item(2)],()=>{calls++;return pending.promise;});
  const original=f.state.items[0];
  const first=f.load([1]),second=f.load([1]);
  assert.equal(calls,1);
  pending.resolve([{id:1,type:'assistant',text:'完整正文'}]);
  await Promise.all([first,second]);
  assert.notEqual(f.state.items[0],original,'Closed group layout must see new item identity');
  assert.equal(f.state.items[0].text,'完整正文');
  assert.equal(f.state.items[1].deferred,true);
});

test('late history does not overwrite a streamed upsert or a different conversation',async()=>{
  for(const switchThread of [false,true]){
    const pending=defer();const f=fixture([item(1)],()=>pending.promise);
    const load=f.load();
    if(switchThread)f.switch();
    f.state.items=[{id:1,type:'assistant',text:'new live content'}];
    pending.resolve([{id:1,type:'assistant',text:'stale disk page'}]);
    await load;
    assert.equal(f.state.items[0].text,'new live content');
  }
});

test('explicit full-history operations page in bounded batches and retry failures',async()=>{
  const calls=[];let fail=true;
  const f=fixture(Array.from({length:600},(_,id)=>item(id)),async(_,ids)=>{
    if(fail){fail=false;throw Error('temporary read failure');}
    calls.push(ids.length);return ids.map(id=>({id,type:'assistant',text:String(id)}));
  });
  await assert.rejects(f.load(),/temporary/);
  await f.load();
  assert.deepEqual(calls,[256,256,88]);
  assert.ok(f.state.items.every(item=>!item.deferred));
  assert.equal(f.state.items[599].text,'599');
});

test('truncated history reloads the index instead of leaving permanent placeholders',async()=>{
  const f=fixture([item(1)],async()=>[]);
  await f.load();
  assert.equal(f.counts().reopened,1);
});

test('live screenshots release frontend Base64 without waiting for a conversation reopen',async()=>{
  const pending=defer();
  const original={id:1,type:'user',text:'screenshot',images:[{mimeType:'image/png',data:'aGVsbG8='}]};
  const state={currentId:'one',items:[original]};
  const code=transformSync(source.slice(source.indexOf('const imageProjectionTickets ='),source.indexOf('function applyUpsert(')),{loader:'ts'}).code;
  let remembered=0;
  const project=new Function('state','api','unwrap','setState','rememberCurrentThreadSnapshot',`let openThreadRequest=1;${code};return projectLiveImages;`)(
    state,{getThreadItems:()=>pending.promise},value=>value,(key,value)=>{state[key]=value;},()=>remembered++);
  project(original);
  pending.resolve([{...original,images:[{mimeType:'image/png',uri:'file:///C:/persistent.png'}]}]);
  await pending.promise; await Promise.resolve();
  assert.equal(state.items[0].images[0].data,undefined);
  assert.equal(state.items[0].images[0].uri,'file:///C:/persistent.png');
  assert.equal(remembered,1);
});
