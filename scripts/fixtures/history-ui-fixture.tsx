import { render } from 'solid-js/web';
import { createMemo, createSignal, Show } from 'solid-js';
import { ChatView } from '../../src/components/ChatView';
import { CanvasTranscript } from '../../src/components/CanvasTranscript';
import { groupItems } from '../../src/components/TurnGroup';
import { state, setState, openThread, closeThread, loadHistoryPage, receiveHistoryNotice, requestHistoryRefresh } from '../../src/store';
import { setWorkspaceLayout } from '../../src/workspaceLayout';
import { pageThread, mergeHistoryPage, windowBytes } from '../../src/historyWindow';
import '../../src/app.css';

const w = window as any;
const records = new Map<string, any>();
const calls: any[] = [], images: any[] = [], details: any[] = [];
let imageActive = 0, imageMax = 0, paints = 0, firstPaint = 0, started = 0, lastFrame = performance.now(), maxFrameGap = 0;
let gate: (() => void) | null = null, gateNext = false, failNext = false, deferredId: string | null = null;
let holdCancel: (() => void) | null = null, cancelShouldFail = false;
const thumb = document.createElement('canvas'); thumb.width = 480; thumb.height = 270;
const ctx = thumb.getContext('2d')!;
ctx.fillStyle = '#14558a'; ctx.fillRect(0, 0, 480, 270); ctx.fillStyle = '#fff'; ctx.font = '24px sans-serif';ctx.fillText('Actual decoded thumbnail', 20, 120);
const thumbUrl = thumb.toDataURL();
const [mode, setMode] = createSignal('chat');
const [canvasThread, setCanvasThread] = createSignal<any>(null);
const canvasGroups = createMemo(previous => groupItems(canvasThread()?.items ?? [], previous), []);
let handle: any;
const tick = (now: number) => { maxFrameGap = Math.max(maxFrameGap, now - lastFrame); lastFrame = now; requestAnimationFrame(tick); };
requestAnimationFrame(tick);
const draw = CanvasRenderingContext2D.prototype.fillText;
CanvasRenderingContext2D.prototype.fillText = function (...args: Parameters<typeof draw>) {
  paints++; if (started && !firstPaint && /(?:prompt-|reply-)/.test(String(args[0]))) firstPaint = performance.now() - started;
  return draw.apply(this, args);
};
function seed(id: string, rounds: number) {
  const items: any[] = [];
  for (let i = 0; i < rounds; i++) {
    const n = i * 4;
    items.push({ type: 'user', id: n + 1, text: `prompt-${i} 中文图片问题`, images: [{ name: `photo-${i}.png`, mimeType: 'image/png', uri: `nova-history://${id}/g-${id}/${n+1}/u/0` }], ts: i + 1 });
    items.push({ type: 'assistant', id: n + 2, text: `reply-${i} **结果**\n` + 'A readable result with code and text. '.repeat(10), ts: i + 1 });
    items.push({ type: 'tool', id: n + 3, toolCallId: `tool-${n}`, title: '截图完成', kind: 'read', status: 'completed', content: [{ type: 'content', content: { type: 'image', mimeType: 'image/png', uri: `nova-history://${id}/g-${id}/${n+3}/p/cA` } }], locations: [], ts: i + 1 });
    items.push({ type: 'turn', id: n + 4, totalTokens: 500, inputTokens: 350, outputTokens: 150, cacheReadTokens: 50, durationMs: 800, stopReason: 'end', ts: i + 1 });
  }
  records.set(id, { id, items, generation: `g-${id}`, users: rounds, turns: rounds, running: false, title: `History ${id}`, updatedAt: 1 });
}
seed('big', 10000); seed('small', 3); seed('anchor', 1000); seed('slow', 5); seed('gallery', 1);
records.get('big').items[4 * 40].text = 'complete-message-'.repeat(20000);
records.get('gallery').items = [{ type:'user',id:1,text:'64-image gallery',ts:1,images:Array.from({length:64},(_,i)=>({name:`gallery-${i}`,mimeType:'image/png',uri:`nova-history://gallery/g-gallery/1/u/${i}`})) }];
const stats = (r: any) => ({ items:r.items.length,users:r.users,turns:r.turns,estimatedBytes:r.items.length*1500,inlineAssetBytes:r.users*4*1024*1024,totalTokens:r.turns*500,inputTokens:r.turns*350,outputTokens:r.turns*150,cacheReadTokens:r.turns*50,cacheWriteTokens:0,maxId:r.items.at(-1)?.id??0 });
const meta = (r: any) => ({ id:r.id,title:r.title,cwd:'/fixture',agentKind:'devin',model:'fixture-model',mode:'build',createdAt:0,updatedAt:r.updatedAt,starred:false,running:r.running,unreadTurns:0 });
function project(r: any, source: any, index: number, full = false) {
  const item = structuredClone(source); item.historyIndex = index;
  if (!full && item.text?.length > 24000) { item.sourceBytes = item.text.length; item.text = item.text.slice(0, 24000); item.detailDeferred = true; }
  if(item.images) item.images=item.images.map((image:any,i:number)=>({...image,uri:`nova-history://${r.id}/${r.generation}/${item.id}/u/${i}`}));
  return item;
}
function pageFor(id: string, request: any = {}) {
  const r=records.get(id); if(!r)throw Error('线程不存在');
  if(request.cursor && !request.cursor.startsWith(r.generation+':'))throw Error('HISTORY_CHANGED: fixture restore');
  const limit=Math.min(request.limit??80,128), n=r.items.length;
  const index=request.aroundId==null?null:r.items.findIndex((i:any)=>i.id===request.aroundId);
  if(index===-1)throw Error('消息不存在');
  const boundary=index!=null?index+1:request.cursor?Number(request.cursor.split(':').at(-1)):n;
  const after=request.direction==='after'&&index==null;
  const start=after?boundary:Math.max(0,boundary-limit),end=after?Math.min(n,boundary+limit):boundary;
  const items=r.items.slice(start,end).map((i:any,k:number)=>project(r,i,start+k));
  return {thread:{...meta(r),items},generation:r.generation,start,end,totalItems:n,turnOffset:r.items.slice(0,start).filter((i:any)=>i.type==='user').length,
    beforeCursor:start?`${r.generation}:${start}`:null,afterCursor:end<n?`${r.generation}:${end}`:null,stats:stats(r),payloadBytes:JSON.stringify(items).length,elapsedMs:1};
}
async function invoke(command: string, args: any = {}) {
  calls.push({command,threadId:args.threadId,time:performance.now(),count:args.ids?.length,original:args.original});
  if(command==='get_thread')throw Error('Full-history IPC must not be used for normal rendering');
  if(command==='get_thread_page') {
    const page=pageFor(args.threadId,args.request);
    if(failNext){failNext=false;throw Error('fixture: disk failure');}
    if(gateNext || deferredId===args.threadId){gateNext=false;deferredId=null;await new Promise<void>(resolve=>{gate=resolve;});}
    return page;
  }
  if(command==='get_thread_display_items') {
    const r=records.get(args.threadId);if(r.generation!==args.generation)throw Error('HISTORY_CHANGED');
    const wanted=new Set(args.ids);return {generation:r.generation,items:r.items.flatMap((i:any,index:number)=>wanted.has(i.id)?[project(r,i,index)]:[]),totalItems:r.items.length,stats:stats(r)};
  }
  if(command==='get_thread_item_detail') {
    const r=records.get(args.threadId);if(args.generation&&args.generation!==r.generation)throw Error('HISTORY_CHANGED');
    details.push(args);const index=r.items.findIndex((i:any)=>i.id===args.itemId);return project(r,r.items[index],index,true);
  }
  if(command==='get_thread_outline') {const r=records.get(args.threadId);return {generation:r.generation,stats:stats(r),prompts:r.items.flatMap((i:any,index:number)=>i.type==='user'?[{id:i.id,text:i.text,index}]:[])};}
  if(command==='get_time_machine_timeline')return {id:'timeline',rootThreadId:args.threadId,currentCheckpointId:null,checkpoints:[]};
  if(command==='get_history_image') {
    if (args.reference === 'nova-history://missing') throw Error('fixture: missing original');
    images.push(args);imageActive++;imageMax=Math.max(imageMax,imageActive);
    await new Promise(resolve=>setTimeout(resolve,20));imageActive--;
    return {attachmentId:args.reference,uri:thumbUrl,thumbnailUri:args.original?null:thumbUrl,width:3840,height:2160,size:2000000};
  }
  if(command==='cancel_turn') {
    await new Promise<void>((resolve,reject)=>{holdCancel=()=>cancelShouldFail?reject(Error('fixture: cancel failed')):resolve();});
    return;
  }
  if(command==='send_prompt') {
    const r=records.get(args.threadId);r.running=true;
    const id=(r.items.at(-1)?.id??0)+1;r.items.push({type:'user',id,text:args.text,images:args.images??[],ts:Date.now()});r.users++;
    receiveHistoryNotice({threadId:r.id,notice:{ids:[id],removed:[],ops:[],chars:0,reset:false}});
    return;
  }
  if(command==='list_threads')return [...records.values()].map(meta);
  if(command==='get_model_options')return {configOptions:[{id:'model',currentValue:'fixture-model',options:[{value:'fixture-model',name:'Fixture model'}]}],modes:{currentModeId:'build',availableModes:[{id:'build',name:'Build'}]}};
  if(command==='get_slash_commands'||command==='list_skills')return [];
  if(command==='plugin:event|listen')return calls.length;
  if(command==='plugin:dialog|message'){w.lastDialog=args;return;}
  if(command==='plugin:webview|internal_toggle_devtools')return;
  if(['report_activity','set_thread_mode','set_thread_unread','plugin:event|unlisten'].includes(command))return;
  throw Error('Unexpected native IPC in fixture: '+command);
}
w.__TAURI_INTERNALS__={invoke,metadata:{currentWindow:{label:'main'},currentWebview:{label:'main'}},transformCallback:()=>calls.length+1,convertFileSrc:(path:string)=>path};
setWorkspaceLayout({open:false});
setState({currentId:null,items:[],threads:[...records.values()].map(meta),agentKind:'devin',model:'fixture-model',mode:'build',theme:'ink-light'});
document.documentElement.dataset.theme='ink-light';
render(()=><div class="app"><Show when={mode()==='chat'} fallback={<div style="width:100%;height:700px;display:flex"><CanvasTranscript ref={value=>handle=value} threadId={canvasThread()?.id??null} groups={canvasGroups()} running={false} loading={false} permissions={[]} preview={false} onReturnToCurrent={()=>{}} emptyHint="fixture"/></div>}><ChatView/></Show></div>,document.getElementById('root')!);
w.perfTest={
 calls,images,details,state,
 open:async(id:string)=>{started=performance.now();firstPaint=0;paints=0;maxFrameGap=0;lastFrame=started;await openThread(id);},
 close:closeThread,
 metadata:()=>state.history,
 metrics:()=>({firstContentPaintMs:firstPaint,firstPaint,paints,maxFrameGap,items:state.items.length,bytes:windowBytes(state.items),images:images.length,imageMax}),
 gate:()=>{gateNext=true;},release:()=>{gate?.();gate=null;},defer:(id:string)=>{deferredId=id;},fail:()=>{failNext=true;},
 cancelAck:(fail=false)=>{cancelShouldFail=fail;holdCancel?.();holdCancel=null;},
 run:(running:boolean)=>{records.get(state.currentId!).running=running;setState('running',state.currentId!,running);},
 notice:(ids:number[]=[],reset=false)=>requestHistoryRefresh(state.currentId!,ids,reset),
 update:(id:number,text:string)=>{const r=records.get(state.currentId!);const i=r.items.find((i:any)=>i.id===id);i.text=text;requestHistoryRefresh(r.id,[id]);},
 restore:()=>{const r=records.get(state.currentId!);r.generation+='r';r.items=r.items.slice(0,40);r.users=r.turns=10;requestHistoryRefresh(r.id,[],true);},
 page:loadHistoryPage,
 canvas:async(id:string)=>{setCanvasThread(pageThread(pageFor(id)));setMode('canvas');},
 canvasAnchor:()=>handle.captureAnchor(),
 canvasStats:()=>({...handle.imageStats(),top:handle.scrollTop(),max:handle.maxScrollTop(),active:handle.activeGroup()}),
 canvasJump:(i:number)=>handle.scrollToGroup(i),
 canvasPage:(direction:'before'|'after')=>{const t=canvasThread(),anchor=handle.captureAnchor();const cursor=direction==='before'?t.history.beforeCursor:t.history.afterCursor;
   setCanvasThread(mergeHistoryPage(t,pageFor(t.id,{cursor,direction}),direction));handle.restoreAnchor(anchor);},
};
