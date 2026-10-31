// Isolated HtmlTranscript regression/performance harness; never connects to the user's browser.
// node scripts/html-transcript.test.mjs --output=/tmp/transcript-baseline
// node scripts/html-transcript.test.mjs --build-only
// --only=deferred,inline-search runs named top-level scenario prefixes (reported as partial coverage).
// --only=scroll-hotpath [--revision=HEAD] profiles frozen git source or the current worktree.
// CPU/trace and text-coverage replays are separate; structural failures are saved, not hidden by timing gates.
// TEST_BROWSER overrides Chromium; TEST_NODE_MODULES supports a sibling worktree.
// Screenshots are review artifacts for content/anchors, not pixel-equality tests.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { access, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { constants } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir, homedir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';
import { installScrollProbe, scrollHotpath } from './transcript-scroll-perf.mjs';

const root = fileURLToPath(new URL('../', import.meta.url));
const args = process.argv.slice(2);
assert.ok(args.every(arg => arg === '--build-only' || arg === '--solid-dev' || /^(--output|--only|--revision)=.+$/.test(arg)), 'Options: --build-only --solid-dev --output=PATH --only=PREFIX,... --revision=HEAD');
const selectedChecks = args.find(arg => arg.startsWith('--only='))?.slice('--only='.length).split(',') ?? [];
const buildOnly = args.includes('--build-only');
const outputArg = args.find(arg => arg.startsWith('--output='))?.slice('--output='.length);
const output = outputArg ? path.resolve(outputArg) : await mkdtemp(path.join(tmpdir(), 'html-transcript-'));
await mkdir(output, { recursive: true });
const revision = args.find(arg => arg.startsWith('--revision='))?.slice('--revision='.length);
let sourceRoot = root;
const sourceCommit = execFileSync('git', ['rev-parse', '--verify', `${revision || 'HEAD'}^{commit}`], { cwd: root, encoding: 'utf8' }).trim();
if (revision) {
  sourceRoot = await mkdtemp(path.join(tmpdir(), 'transcript-revision-'));
  // Read-only git archive: never stash, checkout, or rewrite the working tree.
  const archive = execFileSync('git', ['archive', '--format=tar', sourceCommit, 'src'], { cwd: root, maxBuffer: 64 * 1024 * 1024 });
  // -f - : Windows bsdtar otherwise reads from \\.\tape0 instead of stdin.
  execFileSync('tar', ['-x', '-f', '-', '-C', sourceRoot], { input: archive });
}
const source = { commit: sourceCommit, revision: revision || null, worktree: !revision };
let deps;
for (const candidate of [process.env.TEST_NODE_MODULES, path.join(root, 'node_modules'), path.resolve(root, '../nova-client/node_modules')].filter(Boolean)) {
  try { await access(path.join(candidate, 'esbuild/package.json')); deps = path.resolve(candidate); break; } catch {}
}
assert.ok(deps, 'Missing dependencies: set TEST_NODE_MODULES to an existing node_modules directory');
const require = createRequire(path.join(deps, '__transcript_test__.cjs'));
const { build } = require('esbuild');
const { transformAsync } = require('@babel/core');
const solidPlugin = require('@solidjs/babel-plugin');
const solidDev = process.argv.includes('--solid-dev');
const { chromium } = require('playwright-core');
const limits = { rows: 80, elements: 3000, frameP95Ms: 50, frameMaxMs: 250, anchorPx: 4 };

async function executable() {
  if (process.env.TEST_BROWSER) {
    await access(process.env.TEST_BROWSER, constants.X_OK);
    return process.env.TEST_BROWSER;
  }
  const candidates = [chromium.executablePath(), '/usr/bin/chromium', '/usr/bin/chromium-browser', '/opt/google/chrome/chrome', '/opt/microsoft/msedge/msedge',
    'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe', 'C:/Program Files/Google/Chrome/Application/chrome.exe'];
  const cache = process.env.PLAYWRIGHT_BROWSERS_PATH || path.join(homedir(), '.cache/ms-playwright');
  for (const entry of (await readdir(cache).catch(() => [])).filter(name => /^chromium_headless_shell-\d+$/.test(name)).sort().reverse()) {
    candidates.push(path.join(cache, entry, 'chrome-headless-shell-linux64/chrome-headless-shell'));
  }
  for (const candidate of candidates) {
    try { await access(candidate, constants.X_OK); return candidate; } catch {}
  }
  throw Error('No Chromium found; set TEST_BROWSER (no browser is installed automatically)');
}

function fixture() {
  return `
import { render } from '@solidjs/web';
import { createSignal, createEffect } from 'solid-js';
import { HtmlTranscript as Transcript } from './src/components/HtmlTranscript';
import { state, setState, toggleExpanded } from './src/store';
import { selectedChatText } from './src/chatSelection';
import { resolveUserScrollStick } from './src/scrollStick';
import './src/app.css';
const [session, setSession] = createSignal({id:'empty',groups:[],running:false,loading:false});
let handle, serial=0, stream, streamTicks=0, reads=new Set();
let loadCalls=[], pendingLoads=new Map(), hydrationCount=0, hydrationLarge=false, captureFrames=0, paintedFrames=[], previews=[];
window.addEventListener('nova:preview-file',event=>previews.push(event.detail));
// Mirror ChatView's parent contract, reusing its direction helper. This is not a full App/Tauri fixture.
let stickToBottom=true,lastScrollTop=0,scrollQueued=false,parentPins=0,parentUserScrolls=0;
const cancelBottomFollow=()=>{stickToBottom=false;};
const pinBottom=()=>{if(stickToBottom&&handle){parentPins++;handle.scrollToBottom();lastScrollTop=handle.scrollTop();}};
const scheduleBottomPin=()=>{if(scrollQueued)return;scrollQueued=true;queueMicrotask(()=>{scrollQueued=false;pinBottom();});};
const enableBottomFollow=()=>{stickToBottom=true;scheduleBottomPin();};
const onScroll=(top,max,user)=>{window.__scrollProbe?.notifyScroll(user);if(user){parentUserScrolls++;stickToBottom=resolveUserScrollStick(lastScrollTop,top,max);}lastScrollTop=top;};
let phaseStart=performance.now(), lastFrame=phaseStart, gaps=[], longTasks=[], peakRows=0, peakElements=0;
const host=()=>document.getElementById('fixture');
const dom=()=>({rows:host()?.querySelectorAll('.html-transcript-row[data-group-index]').length||0,
  elements:host()?.querySelectorAll('*').length||0});
const sampleDom=()=>{const d=dom(); peakRows=Math.max(peakRows,d.rows);peakElements=Math.max(peakElements,d.elements);return d;};
const reset=()=>{phaseStart=performance.now();lastFrame=phaseStart;gaps=[];longTasks=[];peakRows=peakElements=0;};
function frame(){const now=performance.now();gaps.push(now-lastFrame);lastFrame=now;requestAnimationFrame(frame);
  if(captureFrames>0){captureFrames--;setTimeout(()=>{
    const visible=[...host().querySelectorAll('[data-group-index]')].filter(row=>{const r=row.getBoundingClientRect(),b=host().getBoundingClientRect();return r.bottom>b.top&&r.top<b.bottom;}).map(row=>Number(row.dataset.groupIndex));
    paintedFrames.push({visible,top:handle?.scrollTop(),max:handle?.maxScrollTop()});
  },0);}
}
window.__scrollProbe?.ignore(frame);
requestAnimationFrame(frame);
if(PerformanceObserver.supportedEntryTypes.includes('longtask'))new PerformanceObserver(list=>{
  longTasks.push(...list.getEntries().filter(e=>e.startTime>=phaseStart).map(e=>e.duration));
}).observe({entryTypes:['longtask']});
const nextFrame=()=>new Promise(resolve=>requestAnimationFrame(resolve));
const stop=()=>{clearInterval(stream);stream=undefined;};
function group(i, prefix='round'){
  const text='answer-'+i+' **bold** 中文 😀 needle-'+i+'-target\\n\\n'+
    Array.from({length:8},(_,j)=>'Paragraph '+j+' '+('readable content '+i+' ').repeat(12)).join('\\n\\n')+
    '\\n\\n- first item\\n- second item\\n\\n~~~js\\nconst round = '+i+';\\n~~~\\n\\nanswer-end-'+i;
  return {user:{type:'user',id:i*3+1,ts:0,text:prefix+'-'+i},
    body:[{type:'assistant',id:i*3+2,ts:0,get text(){reads.add(i);return text;}}],
    turn:{type:'turn',id:i*3+3,ts:0,durationMs:1000,stopReason:'end'}};
}
function replace(groups,kind='session',loading=false){
  stop();streamTicks=0;reads.clear();reset();
  // openThread clears expansion before committing a new snapshot; do not invent transcript ownership.
  for(const key of Object.keys(state.expanded))toggleExpanded(key,false);
  const id=kind+'-'+ ++serial;
  setState(s => { s.currentId = id; });
  setSession({id,groups,running:false,loading});
  enableBottomFollow();
}
const text=()=>host()?.innerText||'';
const anchor=()=>{
  const bounds=document.querySelector('.html-transcript').getBoundingClientRect();
  const rows=[...host().querySelectorAll('.html-transcript-row[data-group-index]')];
  const row=rows.find(el=>{const r=el.getBoundingClientRect();return r.bottom>bounds.top+32&&r.top<bounds.bottom;});
  return row?{index:Number(row.dataset.groupIndex),offset:row.getBoundingClientRect().top-bounds.top}:null;
};
function loadItems(ids){
  loadCalls.push([...ids]);const id=session().id;
  if(pendingLoads.has(id))return pendingLoads.get(id).promise;
  let resolve;
  const promise=new Promise(done=>resolve=done);
  const deferred=new Set(session().groups.flatMap(g=>g.body.filter(item=>item.deferred).map(item=>item.id)));
  pendingLoads.set(id,{promise,release(){
    if(session().id===id){hydrationCount++;setSession(s=>({...s,groups:s.groups.map(g=>g.body.some(item=>deferred.has(item.id))?{...g,body:g.body.map(item=>deferred.has(item.id)?{...item,deferred:false,
      ...(item.type==='tool'?{content:[{type:'content',content:{type:'text',text:'hydrated tool payload deferred-tool-target'}}]}:{text:hydrationLarge?('hydrated **large** thought paragraph\\n\\n').repeat(500):'hydrated **deferred** thought target'})}:item)}:g)}));}
    pendingLoads.delete(id);resolve();
  }});return promise;
}
window.test={
  load(count){replace(Array.from({length:count},(_,i)=>group(i)));},
  position(){return {top:handle.scrollTop(),max:handle.maxScrollTop()};},
  scrollTop(){return handle.scrollTop();},
  async prepareScroll(kind){
    if(kind==='markdown')this.largeMarkdown();
    else if(kind==='dense')replace(Array.from({length:2000},(_,i)=>({body:[{type:'assistant',id:i+1,ts:0,text:'short-message-'+i}]})),'dense-scroll');
    else this.load(2000);
    for(let i=0;i<12;i++)await nextFrame();
    this.jump(kind==='markdown'?0:kind==='dense'?100:750);
    for(let i=0;i<12;i++)await nextFrame();
    if(kind==='markdown')this.scroll(20000);
    let previous='',stable=0;
    for(let i=0;i<120&&stable<8;i++){
      await nextFrame();
      const current=JSON.stringify(this.position());stable=current===previous?stable+1:0;previous=current;
    }
    if(stable<8)throw Error('Scroll geometry did not converge before '+kind);
  },
  scrollAnchor(){return anchor();},
  scrollCoverage(){
    const position=this.position();
    const root=host().querySelector('.html-transcript'),bounds=root.getBoundingClientRect();
    let visibleText=0,example='',visibleRows=0;
    const rowRects=[],emptyChunks=[];
    const space=root.querySelector('.html-transcript-space');
    if(getComputedStyle(space).visibility==='hidden')return {...position,visibleText,visibleRows,example};
    for(const row of root.querySelectorAll('.html-transcript-row[data-group-index]')){
      const box=row.getBoundingClientRect();if(box.bottom<=bounds.top||box.top>=bounds.bottom)continue;
      visibleRows++;
      rowRects.push({index:Number(row.dataset.groupIndex),top:box.top-bounds.top,bottom:box.bottom-bounds.top});
      for(const chunk of row.querySelectorAll('[data-markdown-chunk]')){
        const rect=chunk.getBoundingClientRect();
        if(rect.bottom>bounds.top&&rect.top<bounds.bottom&&!chunk.childElementCount)emptyChunks.push(Number(chunk.dataset.markdownChunk));
      }
      const walker=document.createTreeWalker(row,NodeFilter.SHOW_TEXT);
      let node;
      while((node=walker.nextNode())){
        if(!node.textContent.trim()||node.parentElement.closest('button,[aria-hidden="true"]')||getComputedStyle(node.parentElement).visibility==='hidden')continue;
        const range=document.createRange();range.selectNodeContents(node);
        if([...range.getClientRects()].some(rect=>rect.width>0&&rect.bottom>bounds.top&&rect.top<bounds.bottom)){
          visibleText++;example||=node.textContent.slice(0,80);
        }
      }
    }
    rowRects.sort((a,b)=>a.index-b.index);
    const start=rowRects[0]?.index===0?Math.max(0,24-position.top):0;
    const end=bounds.height-(rowRects.at(-1)?.index===session().groups.length-1?Math.max(0,16-(position.max-position.top)):0);
    let cursor=start,coveredPx=0,maxGapPx=0,maxOverlapPx=0,readingOrder=true;
    for(let i=0;i<rowRects.length;i++){
      const row=rowRects[i],top=Math.max(start,row.top),bottom=Math.min(end,row.bottom);
      maxGapPx=Math.max(maxGapPx,top-cursor);
      if(i){maxOverlapPx=Math.max(maxOverlapPx,Math.min(cursor,bottom)-top);if(row.top<rowRects[i-1].top)readingOrder=false;}
      coveredPx+=Math.max(0,bottom-Math.max(cursor,top));cursor=Math.max(cursor,bottom);
    }
    maxGapPx=Math.max(maxGapPx,end-cursor);
    return {...position,visibleText,visibleRows,example,rowRects,coveredPx,expectedPx:end-start,maxGapPx,maxOverlapPx,readingOrder,emptyChunks};
  },
  visual(){const groups=Array.from({length:3},(_,i)=>({...group(i,'请检查这段代码'),body:[{type:'assistant',id:i*3+2,ts:0,text:'answer-'+i+' **bold** 中文 😀 and [link](https://example.invalid)\\n\\n- Markdown list\\n- native content\\n\\n~~~js\\nconst round = '+i+';\\n~~~\\n\\nanswer-end-'+i}]}));replace(groups,'visual');},
  rich(){
    const g={user:{type:'user',id:101,ts:0,text:'请检查变更并给出结果。\\n保留原生选择、图片和工具详情。',images:[{name:'fixture.svg',mimeType:'image/svg+xml',data:btoa('<svg xmlns="http://www.w3.org/2000/svg" width="240" height="80"><rect width="240" height="80" fill="#41609a"/><text x="20" y="48" fill="white">image fixture</text></svg>')}]},body:[
      {type:'thought',id:102,ts:0,text:'Checking **native DOM** and scroll anchors.'},
      {type:'tool',id:103,ts:0,title:'Read fixture.ts',kind:'read',status:'completed',toolCallId:'visual-read',locations:[],content:[{type:'content',content:{type:'text',text:'export const ready = true;\\nsecond output line'}}]},
      {type:'tool',id:104,ts:0,title:'Check result',kind:'execute',status:'failed',toolCallId:'visual-failed',locations:[],content:[{type:'content',content:{type:'text',text:'exit 1: validation example'}}]},
      {type:'assistant',id:105,ts:0,text:'## 检查结果\\n\\nText with **bold**, *emphasis*, \`inline code\`, and [link](https://example.invalid).\\n\\n1. First numbered item\\n2. Second item\\n   - Nested bullet\\n\\n> A quoted result\\n\\n| Key | Value |\\n| --- | --- |\\n| DOM | native |\\n| Anchor | stable |\\n\\n~~~ts\\nconst ok = true;\\nconsole.log(ok);\\n~~~\\n\\nrich-end'}],turn:{type:'turn',id:106,ts:0,durationMs:1250,totalTokens:1234,stopReason:'end'}};
    replace([g],'rich');
    for(const key of ['turn-106','process-102','thought-102','tool-103','tool-104'])toggleExpanded(key,true);
  },
  inline(){const g=group(0);g.body=[{type:'assistant',id:2,ts:0,text:'Search alpha **bold** omega tail'}];replace([g],'inline');},
  tall(){const groups=Array.from({length:60},(_,i)=>group(i));groups[30]={...groups[30],body:[{type:'assistant',id:92,ts:0,text:Array.from({length:700},(_,i)=>'tall-line-'+i+' stable reading anchor').join('\\n\\n')}]};replace(groups,'tall');},
  firstPaint(){paintedFrames=[];replace(Array.from({length:2000},(_,i)=>group(i)),'first-paint');captureFrames=8;},
  paintedFrames(){return paintedFrames;},
  deferred(kind){
    loadCalls=[];hydrationCount=0;hydrationLarge=kind==='above';const groups=Array.from({length:80},(_,i)=>group(i));
    groups[30]={user:{type:'user',id:9000,text:'deferred-round',ts:0},body:[
      {type:'thought',id:9001,ts:0,text:'',deferred:true},
      {type:'tool',id:9002,ts:0,title:'fixture tool',kind:'execute',status:'completed',toolCallId:'fixture-tool',content:[],locations:[],deferred:true},
      {type:'assistant',id:9003,ts:0,text:'deferred conclusion'}],turn:{type:'turn',id:9004,ts:0,durationMs:1000,stopReason:'end'}};
    for(const key of ['turn-9004','process-9001','process-9002','thought-9001','tool-9002'])toggleExpanded(key,false);
    replace(groups,'deferred-'+kind);
    if(kind==='above')for(const key of ['turn-9004','process-9001','thought-9001'])toggleExpanded(key,true);
  },
  releaseLoads(){for(const pending of [...pendingLoads.values()])pending.release();},
  loadStats(){return {calls:loadCalls,pending:pendingLoads.size,hydrationCount};},
  loadFold(kind='paragraphs'){
    const groups=Array.from({length:12},(_,i)=>group(i));
    const payload=kind==='continuous'?'长'.repeat(724000):('large prompt 中文 😀 '+ 'paragraph '.repeat(12)+'\\n').repeat(6000).slice(0,724000);
    groups[4]={...groups[4],user:{...groups[4].user,text:'fold-start '+payload+' fold-search-target'}};
    toggleExpanded('user-text-13',false);replace(groups,'fold-'+kind);
  },
  loadSingle(kind){
    const g=group(0,'single');
    if(kind==='reply')g.body=[{type:'assistant',id:2,ts:0,text:'huge-reply-start\\n\\n'+('large answer **bold** 中文\\n\\n').repeat(12000)+'huge-reply-end'}];
    else g.user={...g.user,text:'huge-prompt-start '+(kind==='continuous'?'长'.repeat(724000):('large prompt 中文 😀\\n').repeat(50000).slice(0,724000))+' huge-prompt-end'};
    toggleExpanded('user-text-1',false);replace([g],'single-'+kind);
  },
  largeMarkdown(){
    const g=group(0,'large-markdown');
    g.body=[{type:'assistant',id:2,ts:0,text:Array.from({length:12000},(_,i)=>
      'markdown-paragraph-'+i+' **bold** '+(i>=5950&&i<=6150
        ? (i%8===0?'reading content '.repeat(10):'')
        : 'large answer 中文 '+('reading content ').repeat(2+i%7))+
      (i===24?' markdown-early-unique':i===6000?' markdown-middle-6000 alpha **bold** *emphasis* \`code\` omega':i===11976?' markdown-end-unique':'')
    ).join('\\n\\n')+'\\n\\nlarge-markdown-tail'}];
    replace([g],'large-markdown');
  },
  markdownSnapshot(capture=false){
    const root=document.querySelector('.html-transcript'),top=root.getBoundingClientRect().top;
    const chunks=[...root.querySelectorAll('[data-markdown-chunk]')];
    if(capture)window.markdownReading=chunks.flatMap(chunk=>[...chunk.children]).find(el=>{
      const rect=el.getBoundingClientRect();return rect.bottom>top&&rect.top<top+root.clientHeight;
    });
    const element=window.markdownReading,chunk=element?.closest('[data-markdown-chunk]');
    return {chunks:chunks.length,mounted:chunks.filter(el=>el.childElementCount).map(el=>Number(el.dataset.markdownChunk)),
      reading:element?{text:element.textContent,connected:element.isConnected,chunk:Number(chunk?.dataset.markdownChunk),
        offset:element.getBoundingClientRect().top-top,inset:element.getBoundingClientRect().top-(chunk?.getBoundingClientRect().top||0),
        chunkHeight:chunk?.getBoundingClientRect().height}:null};
  },
  switchRace(){handle.scrollToGroup(0);replace([group(0,'replacement')],'replacement');},
  fileLinks(){
    previews=[];const groups=Array.from({length:3},(_,i)=>group(i,'links'));
    groups[2].body=[{type:'assistant',id:8,ts:0,text:'links [\`a.ts\`](D:/proj/src/a.ts) [b](b.ts:12) [c](file:///D:/x/c.ts#L3) [web](https://example.invalid) see src/d.ts:5\\n\\nlinks-end'}];
    replace(groups,'links');
  },
  previews(){return previews;},
  // Samples each painted frame after a session switch (rAF, then a task after that frame's paint).
  async switchFrames(kind){
    const count={short:3,images:40}[kind]??2000,frames=[];
    const groups=Array.from({length:count},(_,i)=>group(i,kind));
    if(kind==='images')for(const [i,g] of groups.slice(-6).entries())g.body=[{...g.body[0],text:'image answer '+i+'\\n\\n![chart '+i+']('+location.origin+'/img-'+i+'.svg)\\n\\nimage-end-'+i}];
    // The last turn exceeds the 128-item history tail: its process items arrive deferred, like getThreadView;
    // the previous turn's answer is deferred too and is visible above it.
    if(kind==='deferred'){const g=groups.at(-1),prev=groups.at(-2);g.body=[{type:'thought',id:900001,ts:0,text:'',deferred:true},{type:'tool',id:900002,ts:0,title:'fixture tool',kind:'execute',status:'completed',toolCallId:'tail-tool',content:[],locations:[],deferred:true},...g.body];prev.body=[{...prev.body[0],text:'',deferred:true}];}
    replace(groups,'switch-'+kind);
    if(kind==='deferred')setTimeout(()=>this.releaseLoads(),50);
    for(let i=0;i<10;i++){
      await new Promise(resolve=>requestAnimationFrame(()=>setTimeout(resolve,0)));
      const root=host().querySelector('.html-transcript'),space=root.querySelector('.html-transcript-space');
      const c=this.scrollCoverage();
      frames.push({hidden:getComputedStyle(space).visibility==='hidden',status:root.querySelector('[role=status]')?.textContent||'',placeholder:!!root.querySelector('[data-history-loading]'),
        top:Math.round(c.top),max:Math.round(c.max),rows:(c.rowRects||[]).map(r=>r.index),gap:Math.round(c.maxGapPx??0),overlap:Math.round(c.maxOverlapPx??0),
        stale:[...root.querySelectorAll('.ht-user-text')].some(el=>!el.textContent.startsWith(kind+'-'))});
    }
    return frames;
  },
  cold(){replace([],'cold',true);},
  finishCold(){setSession(s=>({...s,groups:[group(0,'cold-ready')],loading:false}));},
  jump(index){cancelBottomFollow();handle.scrollToGroup(index);}, bottom(){enableBottomFollow();pinBottom();},
  scroll(delta){handle.scrollBy(delta);}, search(id,query){cancelBottomFollow();handle.revealMatch(id,query);},
  expanded(id){return !!state.expanded['user-text-'+id];},
  startStream(){
    stop();streamTicks=0;
    setSession(s=>({...s,running:true,groups:s.groups.map((g,i)=>i===s.groups.length-1?{...g,turn:undefined}:g)}));
    const id=session().id;
    stream=setInterval(()=>{
      if(session().id!==id){stop();return;}
      streamTicks++;
      setSession(s=>({...s,groups:s.groups.map((g,i)=>i===s.groups.length-1?{...g,body:[{...g.body[0],text:g.body[0].text+'\\n\\nstream-'+streamTicks+' '+ 'fresh content '.repeat(40)}]}:g)}));
    },60);
  },
  stopStream(){stop();setSession(s=>({...s,running:false}));},
  text, anchor, reset, sampleDom, selectedChatText,
  highlights(){return [...(CSS.highlights?.get('transcript-search')||[])].map(range=>({text:range.toString(),top:range.getBoundingClientRect().top}));},
  sameIds(label){const groups=session().groups.map(g=>({...g,user:g.user?{...g.user,text:label+' '+g.user.text}:undefined}));replace(groups,label);},
  stats(){const d=sampleDom();return {...d,peakRows,peakElements,reads:reads.size,readIndices:[...reads],
    top:handle?.scrollTop(),max:handle?.maxScrollTop(),active:handle?.activeGroup(),bottom:handle?.isAtBottom(),
    focused:handle?.hasFocusedInput(),streamTicks,parentPins,parentUserScrolls,stickToBottom,elapsed:performance.now()-phaseStart,gaps:[...gaps],longTasks:[...longTasks]};},
  async scrollFrames(count=180){
    cancelBottomFollow();handle.scrollToGroup(Math.floor(session().groups.length/2));
    for(let i=0;i<8;i++)await nextFrame();
    reset();
    for(let i=0;i<count;i++){
      handle.scrollBy(i<count/2?90:-90);
      await nextFrame();
      if(i%12===0)sampleDom();
    }
    return this.stats();
  }
};
render(()=>{
  createEffect(session,scheduleBottomPin);
  window.addEventListener('keydown',event=>{
    if(event.altKey||event.ctrlKey||event.metaKey||handle?.hasFocusedInput())return;
    const up=['ArrowUp','PageUp','Home'].includes(event.key)||(event.key===' '&&event.shiftKey);
    const down=['ArrowDown','PageDown','End'].includes(event.key)||(event.key===' '&&!event.shiftKey);
    if(!up&&!down)return;event.preventDefault();handle.scrollBy(down?100:-100);
    if(down&&handle.isAtBottom()&&!stickToBottom)enableBottomFollow();else if(!handle.isAtBottom())cancelBottomFollow();
  },true);
  if(!window.__scrollProbe)queueMicrotask(()=>new MutationObserver(sampleDom).observe(host(),{childList:true,subtree:true}));
  return <div id="fixture"><Transcript ref={value=>{handle=value;scheduleBottomPin();}} threadId={session().id}
    groups={session().groups} running={session().running} loading={session().loading} permissions={[]} loadItems={loadItems}
    onScroll={onScroll} onBrowseDetail={cancelBottomFollow}
    preview={false} readOnly={false} onReturnToCurrent={()=>{}} emptyHint="empty" /></div>;
},document.getElementById('root'));
`;
}

async function solid(source, filename) {
  const result = await transformAsync(source, { filename, babelrc: false, configFile: false,
    plugins: [[solidPlugin, { generate: 'dom', hydratable: false, dev: solidDev }]], parserOpts: { plugins: ['jsx', 'typescript'] } });
  return result.code;
}

async function bundle() {
  const outdir = path.join(output, '.bundle');
  return build({ absWorkingDir: sourceRoot, stdin: { contents: await solid(fixture(), path.join(sourceRoot, 'fixture.tsx')), resolveDir: sourceRoot, sourcefile: 'fixture.tsx', loader: 'ts' },
    bundle: true, write: false, outdir, entryNames: 'bundle', platform: 'browser', format: 'esm', target: 'es2022',
    conditions: solidDev ? ['browser', 'development'] : ['browser'], nodePaths: [deps], define: { 'import.meta.env.DEV': String(solidDev), 'import.meta.env.PROD': String(!solidDev) },
    loader: { '.woff2': 'file', '.woff': 'file', '.ttf': 'file', '.svg': 'file', '.png': 'file', '.webp': 'file' },
    plugins: [{ name: 'solid', setup(builder) {
      builder.onLoad({ filter: /\.[jt]sx$/ }, async ({ path: filename }) => ({ contents: await solid(await readFile(filename, 'utf8'), filename), loader: 'ts' }));
    } }],
  });
}

function summarize(stats) {
  const sorted = stats.gaps.slice().sort((a, b) => a - b);
  const percentile = p => sorted[Math.min(sorted.length - 1, Math.ceil(sorted.length * p) - 1)] ?? 0;
  return { elapsedMs: stats.elapsed, frames: sorted.length, frameP95Ms: percentile(.95), frameP99Ms: percentile(.99),
    frameMaxMs: Math.max(0, ...sorted), longTasks: stats.longTasks.length, longestTaskMs: Math.max(0, ...stats.longTasks),
    rows: stats.rows, elements: stats.elements, peakRows: stats.peakRows, peakElements: stats.peakElements, readGroups: stats.reads };
}
function checkBounded(stats, label) {
  assert.ok(stats.peakRows <= limits.rows, `${label}: ${stats.peakRows} mounted rows > ${limits.rows}`);
  assert.ok(stats.peakElements <= limits.elements, `${label}: ${stats.peakElements} DOM elements > ${limits.elements}`);
}
function checkFrames(stats, label) {
  const m = summarize(stats);
  assert.ok(m.frames >= 30, `${label}: insufficient frames (${m.frames})`);
  assert.ok(m.frameP95Ms <= limits.frameP95Ms, `${label}: p95 ${m.frameP95Ms.toFixed(1)}ms > ${limits.frameP95Ms}ms`);
  assert.ok(m.frameMaxMs <= limits.frameMaxMs, `${label}: worst frame ${m.frameMaxMs.toFixed(1)}ms > ${limits.frameMaxMs}ms`);
}

async function run(browser, port, report) {
  const context = await browser.newContext({ viewport: { width: 1280, height: 800 }, deviceScaleFactor: 1, reducedMotion: 'reduce' });
  // Fixtures contain no remote images: prevent accidental IPC/provider/network side effects.
  await context.route('**/*', route => new URL(route.request().url()).origin === `http://127.0.0.1:${port}` ? route.continue() : route.abort());
  if (selectedChecks.some(prefix => prefix.startsWith('scroll-hotpath'))) await context.addInitScript(installScrollProbe);
  const page = await context.newPage();
  report.parentContract = 'ChatView onScroll/direction, microtask bottom pin, browse-detail cancellation, input keyboard guard; no full App/Tauri backend';
  const errors = [];
  report.checks = [];
  page.setDefaultTimeout(10000);
  page.on('pageerror', error => errors.push(error.message));
  page.on('console', message => {
    if (/\[(?:FLUSH_IN_EFFECT_CALLBACK|PRIMITIVE_IN_EFFECT_CALLBACK|REACTIVE_WRITE_IN_OWNED_SCOPE)\]/.test(message.text())) errors.push(message.text());
  });
  const waitText = text => page.waitForFunction(text => window.test?.text().includes(text), text, { timeout: 15000 });
  const stats = () => page.evaluate(() => window.test.stats());
  const settle = () => page.evaluate(async () => { for (let i = 0; i < 8; i++) await new Promise(requestAnimationFrame); });
  const waitBottom = () => page.waitForFunction(() => { const s=window.test.stats();return s.max-s.top<=2; });
  const screenshot = name => page.locator('#fixture').screenshot({ path: path.join(output, `${name}.png`) });
  let checkDepth = 0;
  async function check(name, fn) {
    if (!checkDepth && selectedChecks.length && !selectedChecks.some(prefix => name.startsWith(prefix))) {
      report.checks.push({ name, skipped: true }); return;
    }
    checkDepth++;
    try { await fn(); report.checks.push({ name, passed: true }); }
    catch (error) {
      const s = await stats().catch(() => null);
      report.checks.push({ name, passed: false, error: error.message, stats: s ? { ...summarize(s), active: s.active, top: s.top, max: s.max } : null });
      await screenshot(`failure-${name}`).catch(() => {});
    } finally { checkDepth--; }
  }
  async function openEditor(index) {
    await page.locator(`[data-group-index="${index}"]`).getByRole('button', { name: '编辑消息', exact: true }).click();
  }
  async function clickLabel(label) {
    await page.locator('#fixture').getByRole('button', { name: new RegExp(label) }).first().click();
  }
  try {
    await page.goto(`http://127.0.0.1:${port}/`, { waitUntil: 'load' });
    await page.waitForFunction(() => window.test?.stats().top !== undefined);
    assert.equal(await page.locator('#fixture canvas').count(), 0, 'HTML transcript must not paint text into a canvas');
    assert.equal(await page.locator('.html-transcript').getAttribute('tabindex'), '0', 'Transcript must be keyboard focusable');
    if (selectedChecks.some(prefix => prefix.startsWith('scroll-hotpath'))) {
      await check('scroll-hotpath', () => scrollHotpath({ page, context, output, report, check, settle }));
    }
    for (const theme of ['ink-dark', 'ink-light']) for (const width of [1280, 980]) {
      await check(`visual-${theme}-${width}`, async () => {
        await page.setViewportSize({ width, height: 800 });
        await page.evaluate(theme => { document.documentElement.dataset.theme=theme;window.test.visual(); }, theme);
        await waitText('answer-end-2');
        await settle();
        await page.evaluate(() => window.test.bottom());
        await waitBottom();
        await screenshot(`${theme}-${width}`);
        await page.evaluate(() => window.test.rich());
        await page.evaluate(() => window.test.jump(0));
        await waitText('请检查变更');
        await settle();
        await screenshot(`rich-${theme}-${width}-top`);
        await page.evaluate(() => window.test.bottom());
        await waitText('rich-end');
        await waitBottom();
        await screenshot(`rich-${theme}-${width}-bottom`);
      });
    }
    await page.setViewportSize({ width: 1280, height: 800 });
    await page.evaluate(() => document.documentElement.dataset.theme='ink-dark');
    for (const count of [2000, 4000]) await check(`long-session-${count}`, async () => {
      await page.evaluate(count => window.test.load(count), count);
      await waitText(`answer-end-${count - 1}`);
      await waitBottom();
      await settle();
      const loaded = await stats();
      checkBounded(loaded, `${count}/initial`);
      assert.ok(loaded.reads < 100, `Offscreen history eagerly read: ${loaded.reads}/${count}`);
      report.scenarios.push({ name: `load-${count}`, ...summarize(loaded) });
      assert.equal(await page.locator('#fixture canvas').count(), 0, 'Loaded HTML transcript must remain canvas-free');
      assert.ok(loaded.rows > 0 && loaded.rows < count, 'Real DOM turns must be mounted and virtualized');
      assert.ok(await page.locator('.html-transcript-row').filter({ hasText: `answer-${count - 1}` }).count(), 'Visible answer must exist as DOM text');
      await screenshot(`${count}-bottom`);
      for (const index of [0, Math.floor(count / 2), count - 1]) await check(`jump-${count}-${index}`, async () => {
        await page.evaluate(index => window.test.jump(index), index);
        await waitText(`round-${index}`);
        await settle();
        const jumped = await stats();
        if (index !== count - 1) assert.equal(jumped.active, index, 'Jump must retain exact group anchor');
        checkBounded(jumped, `${count}/jump-${index}`);
      });
      const metrics = await page.evaluate(() => window.test.scrollFrames());
      report.scenarios.push({ name: `scroll-${count}`, ...summarize(metrics) });
      checkBounded(metrics, `${count}/scroll`);
      checkFrames(metrics, `${count}/scroll`);
      await page.evaluate(count => window.test.jump(count / 2), count);
      await waitText(`round-${count / 2}`);
      await settle();
      await screenshot(`${count}-middle`);
    });

    await check('stream-follow-and-browse', async () => {
    if (selectedChecks.length) { await page.evaluate(() => window.test.load(2000)); await waitText('answer-end-1999'); }
    await page.evaluate(() => window.test.bottom());
    await waitBottom();
    const beforeStream = await stats();
    await page.evaluate(() => window.test.startStream());
    await page.waitForFunction(() => window.test.stats().streamTicks >= 12);
    await waitBottom();
    assert.ok((await stats()).max > beforeStream.max, 'Streaming fixture must actually grow');
    const bounds = await page.locator('#fixture').boundingBox();
    await page.mouse.move(bounds.x + bounds.width / 2, bounds.y + bounds.height / 2);
    await page.mouse.wheel(0, -900);
    await page.waitForFunction(() => window.test.stats().max - window.test.stats().top > 100);
    await settle();
    const browsing = await stats();
    await page.waitForFunction(n => window.test.stats().streamTicks >= n + 12, browsing.streamTicks);
    const streamed = await stats();
    assert.ok(streamed.parentPins>0 && streamed.parentUserScrolls>0,'Parent pin/scroll integration was not exercised');
    assert.equal(streamed.stickToBottom,false,'Parent bottom-follow did not cancel on upward browse');
    assert.ok(Math.abs(streamed.top - browsing.top) <= limits.anchorPx, `Streaming stole free-browse scroll: ${browsing.top} -> ${streamed.top}`);
    await page.evaluate(() => window.test.stopStream());
    await screenshot('stream-free-browse');
    });
    await page.evaluate(() => window.test.stopStream());

    await check('resize', async () => {
    if (selectedChecks.length) { await page.evaluate(() => window.test.load(2000)); await waitText('answer-end-1999'); }
    await page.evaluate(() => window.test.jump(1900));
    await waitText('round-1900');
    await settle();
    const anchorBefore = await page.evaluate(() => window.test.anchor());
    await page.setViewportSize({ width: 980, height: 720 });
    await settle();
    assert.equal((await stats()).active, 1900, 'Resize must preserve the active reading group');
    if (anchorBefore) {
      const after = await page.evaluate(() => window.test.anchor());
      assert.equal(after?.index, anchorBefore.index, 'Resize replaced the DOM anchor');
      assert.ok(Math.abs(after.offset - anchorBefore.offset) <= limits.anchorPx, 'Resize changed the DOM anchor offset');
    }
    await screenshot('resized-anchor');
    });
    await page.setViewportSize({ width: 1280, height: 800 });
    await check('first-paint-tail', async () => {
      await page.evaluate(() => window.test.firstPaint());
      await page.waitForFunction(() => window.test.paintedFrames().length>=8);
      const frames=await page.evaluate(() => window.test.paintedFrames());
      report.firstVisibleFrames=frames;
      const first=frames.find(frame=>frame.visible.length);
      assert.ok(first, 'No visible transcript frame sampled');
      assert.ok(first.visible.includes(1999), `First visible frame showed history instead of tail: ${JSON.stringify(first)}`);
      assert.ok(first.max-first.top<=2, 'First visible content frame was not pinned to tail');
    });
    await check('resize-tall-group', async () => {
      await page.evaluate(() => window.test.tall());
      await page.evaluate(() => window.test.search(92,'tall-line-350 stable reading anchor'));
      await page.waitForFunction(() => window.test.highlights().some(hit=>hit.text==='tall-line-350 stable reading anchor'));
      await settle();
      const row=page.locator('[data-group-index="30"]');
      assert.ok((await row.boundingBox()).height>5000,'Tall fixture must exceed 5000px');
      const before=await page.evaluate(() => window.test.highlights()[0]);
      assert.ok(before.top>=0&&before.top<700,'Tall group midpoint was not visible');
      await page.setViewportSize({width:980,height:720});
      await settle();
      const after=await page.evaluate(() => window.test.highlights()[0]);
      assert.equal((await stats()).active,30,'Resize lost the tall group');
      assert.ok(after && Math.abs(after.top-before.top)<=limits.anchorPx,`Resize lost tall-group internal anchor: ${before.top} -> ${after?.top}`);
      await screenshot('resize-tall-group');
    });
    await page.setViewportSize({width:1280,height:800});
    await check('search', async () => {
    await page.evaluate(() => window.test.load(2000)); await waitText('answer-end-1999');
    await page.evaluate(() => window.test.search(3692, 'NEEDLE-1230-TARGET'));
    await waitText('needle-1230-target');
    await settle();
    const hits = await page.evaluate(() => window.test.highlights());
    assert.ok(hits.some(hit => /needle-1230-target/i.test(hit.text)), 'Search must create native CSS Highlight ranges');
    assert.ok(hits.some(hit => hit.top >= 0 && hit.top < 700), 'Search highlight must be in the viewport');
    await screenshot('search');
    });

    await check('session-switch-frames', async () => {
      // Every painted frame after a switch shows only the new session, pinned to its tail with final geometry:
      // no stale rows, no history placeholders swapping in place, no cold image growth after reveal.
      await page.evaluate(() => window.test.load(2000)); await waitText('answer-end-1999'); await waitBottom(); await settle();
      const log = report.switchFrames = {};
      for (const [n, [from, to]] of [['long','next'],['next','short'],['short','long'],['long','images'],['images','long'],['long','images'],['images','deferred']].entries()) {
        const frames = await page.evaluate(kind => window.test.switchFrames(kind), to);
        log[`${n}:${from}->${to}`] = frames;
        const shown = frames.filter(f => !f.hidden);
        assert.ok(shown.length, `${from}->${to}: never revealed`);
        assert.ok(frames.findIndex(f => !f.hidden) <= frames.findLastIndex(f => f.hidden) + 1, `${from}->${to}: hidden again after reveal`);
        for (const f of frames) assert.ok(!f.stale, `${from}->${to}: stale session text painted ${JSON.stringify(frames)}`);
        for (const f of shown) {
          assert.ok(!f.placeholder, `${from}->${to}: history placeholder painted ${JSON.stringify(frames)}`);
          assert.ok(f.max - f.top <= 2 && f.gap <= 2 && f.overlap <= 2, `${from}->${to}: unpinned or broken frame ${JSON.stringify(frames)}`);
        }
        assert.ok(shown.every(f => JSON.stringify(f.rows) === JSON.stringify(shown[0].rows) && f.top === shown[0].top), `${from}->${to}: geometry moved after reveal ${JSON.stringify(frames)}`);
        await settle();
      }
    });

    await check('file-links-and-menu', async () => {
      await page.evaluate(() => window.test.fileLinks());
      await waitText('links-end');
      await settle();
      const refs = page.locator('#fixture .md-file-ref');
      // Sanitizing must not drop drive-letter/file:/`name:line` targets or shorten a linked label to its basename.
      assert.deepEqual(await refs.evaluateAll(els => els.map(el => [el.dataset.path, el.dataset.line ?? null])),
        [['D:/proj/src/a.ts', null], ['b.ts', '12'], ['D:/x/c.ts', '3'], ['src/d.ts', '5']]);
      await refs.nth(0).click();
      await refs.nth(1).click();
      assert.deepEqual(await page.evaluate(() => window.test.previews()), [{ path: 'D:/proj/src/a.ts', line: undefined }, { path: 'b.ts', line: 12 }]);
      const menuAt = async (x, y) => {
        await refs.nth(2).evaluate((el, [x, y]) => el.dispatchEvent(new MouseEvent('contextmenu', { clientX: x, clientY: y, button: 2, bubbles: true, cancelable: true })), [x, y]);
        return page.locator('.ctx-menu').evaluate(el => {
          const r = el.getBoundingClientRect(), hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
          return { body: el.parentElement === document.body || el.parentElement?.parentElement === document.body, topmost: el.contains(hit), left: r.left, top: r.top, right: r.right, bottom: r.bottom, w: innerWidth, h: innerHeight };
        });
      };
      const box = await refs.nth(2).boundingBox();
      const menu = await menuAt(box.x + 4, box.y + 4);
      assert.ok(menu.body && menu.topmost, `File menu must be a top-level layer above transcript rows: ${JSON.stringify(menu)}`);
      assert.ok(Math.abs(menu.left - box.x - 4) <= 1 && Math.abs(menu.top - box.y - 4) <= 1, `File menu must open at the pointer: ${JSON.stringify(menu)}`);
      await page.keyboard.press('Escape');
      const edge = await menuAt(1279, 799);
      assert.ok(edge.right <= edge.w && edge.bottom <= edge.h && edge.left >= 0 && edge.top >= 0, `File menu must stay inside the viewport: ${JSON.stringify(edge)}`);
      await page.keyboard.press('Escape');
    });

    await check('session-switch-race', async () => {
    // Queue an old layout, then replace the thread before its asynchronous work settles.
    await page.evaluate(() => window.test.switchRace());
    await waitText('replacement-0');
    await settle();
    assert.equal((await stats()).active, 0, 'Old session work replaced the new session');
    assert.ok(!(await page.evaluate(() => window.test.text())).includes('round-1230'), 'Stale thread text leaked');
    await screenshot('switch');
    await page.evaluate(() => window.test.cold());
    await settle();
    await page.evaluate(() => window.test.finishCold());
    await waitText('cold-ready-0');
    await waitBottom();
    });

    // Fold/unfold and search in content that starts outside the collapsed preview.
    for (const kind of ['continuous', 'paragraphs']) await check(`fold-${kind}`, async () => {
      await page.evaluate(kind => window.test.loadFold(kind), kind);
      await page.evaluate(() => window.test.jump(4));
      await waitText('fold-start');
      await settle();
      const collapsed = await stats();
      await page.evaluate(() => window.test.reset());
      await clickLabel('展开完整提示词');
      await page.waitForFunction(() => window.test.expanded(13));
      await page.waitForFunction(max => window.test.stats().max > max + 1000, collapsed.max, { timeout: 60000 });
      await settle();
      const expanded = await stats();
      report.scenarios.push({ name: `expanded-${kind}`, ...summarize(expanded) });
      assert.equal(expanded.active, 4, 'Expansion must not jump to the streaming tail');
      await page.evaluate(() => window.test.search(13, 'fold-search-target'));
      await waitText('fold-search-target');
      await screenshot(`expanded-${kind}`);
      await clickLabel('收起长提示词');
      await page.waitForFunction(() => !window.test.expanded(13));
      await settle();
      assert.equal((await stats()).active, 4, 'Collapse must return to the original group');
      assert.ok(summarize(expanded).frameMaxMs <= limits.frameMaxMs, `Expanded 724k ${kind}: ${summarize(expanded).frameMaxMs.toFixed(1)}ms main-thread gap > ${limits.frameMaxMs}ms`);
    });

    for (const kind of ['continuous', 'paragraphs', 'reply']) await check(`single-${kind}`, async () => {
      await page.evaluate(kind => window.test.loadSingle(kind), kind);
      if (kind !== 'reply') await page.evaluate(() => window.test.jump(0));
      await waitText(kind === 'reply' ? 'huge-reply-end' : 'huge-prompt-start');
      await settle();
      const loaded = await stats();
      report.scenarios.push({ name: `single-${kind}`, ...summarize(loaded) });
      checkBounded(loaded, `single-${kind}`);
      assert.ok(summarize(loaded).frameMaxMs <= 500, `Single ${kind} blocks the main thread`);
      await screenshot(`single-${kind}`);
    });

    const loadMarkdown = async () => {
      await page.evaluate(() => window.test.largeMarkdown());
      await waitText('large-markdown-tail');
      await waitBottom();
      await settle();
      assert.equal(await page.locator('[data-markdown-virtual]').count(), 1, 'Large reply must use the Markdown window');
      checkBounded(await stats(), 'large-markdown/initial');
    };
    const markdownSnapshot = capture => page.evaluate(capture => window.test.markdownSnapshot(capture), !!capture);
    const scrollMarkdown = (index, position) => page.evaluate(({index,position}) => {
      const root=document.querySelector('.html-transcript');
      const chunk=root.querySelector(`[data-markdown-chunk="${index}"]`);
      if (!chunk?.childElementCount) throw Error(`Expected mounted reading chunk ${index}`);
      const blocks=[...chunk.children],block=blocks[Math.floor((blocks.length-1)*position)];
      window.test.scroll(block.getBoundingClientRect().top-root.getBoundingClientRect().top+8);
      return window.test.markdownSnapshot(true);
    }, {index,position});
    const assertMarkdownAnchor = (before, after, label) => {
      assert.ok(before.reading && after.reading?.connected, `${label}: reading block unmounted`);
      assert.equal(after.reading.text, before.reading.text, `${label}: reading block replaced`);
      assert.ok(Math.abs(after.reading.offset-before.reading.offset)<=limits.anchorPx,
        `${label}: chunk ${before.reading.chunk}, inset ${before.reading.inset}, offset ${before.reading.offset} -> ${after.reading.offset}`);
    };
    for (const [name,query] of [
      ['early','markdown-early-unique'], ['middle','markdown-middle-6000'], ['end','markdown-end-unique'],
      ['inline','markdown-middle-6000 alpha bold emphasis code omega'],
    ]) await check(`large-markdown-search-${name}`, async () => {
      await loadMarkdown();
      if (name==='end') {
        await page.evaluate(() => window.test.jump(0));
        await waitText('markdown-paragraph-0 ');
        await settle();
      }
      assert.ok(!(await page.evaluate(() => window.test.text())).includes(query), `${name}: search fixture must start with a hidden match`);
      await page.evaluate(query => window.test.search(2,query), query);
      await page.waitForFunction(query => window.test.highlights().some(hit=>hit.text===query), query);
      await settle();
      const hits=await page.evaluate(() => window.test.highlights());
      const state=await markdownSnapshot(true),metrics=await stats();
      report.scenarios.push({name:`large-markdown-search-${name}`,query,hits,...state,...summarize(metrics)});
      assert.equal(hits.filter(hit=>hit.text===query).length,1, `${name}: expected one unique native highlight`);
      assert.ok(hits.some(hit=>hit.text===query&&hit.top>=0&&hit.top<700), `${name}: hidden chunk match is still offscreen: ${JSON.stringify(hits)}`);
      assert.ok(state.mounted.length<state.chunks, `${name}: search mounted every chunk`);
      checkBounded(metrics, `large-markdown/search-${name}`);
      await screenshot(`large-markdown-search-${name}`);
    });
    await check('large-markdown-scroll-anchor', async () => {
      await loadMarkdown();
      await page.evaluate(() => window.test.search(2,'markdown-middle-6000'));
      await page.waitForFunction(() => window.test.highlights().some(hit=>hit.text==='markdown-middle-6000'));
      await settle();
      await page.evaluate(() => document.querySelector('[data-markdown-virtual]').dispatchEvent(new CustomEvent('markdown-reveal',{detail:{query:''}})));
      await settle();
      const initial=await markdownSnapshot(true),trace=[];
      assert.ok(initial.reading?.chunk>0, 'Scroll must start in a deep Markdown chunk');
      report.scenarios.push({name:'large-markdown-scroll-anchor',trace});
      let index=initial.reading.chunk,loads=0,unloads=0;
      for (const direction of [1,1,1,-1,-1,-1]) {
        const previous=await markdownSnapshot();
        const before=await scrollMarkdown(index,direction>0?1:0);
        await settle();
        const after=await markdownSnapshot(),metrics=await stats();
        loads+=after.mounted.filter(n=>!previous.mounted.includes(n)).length;
        unloads+=previous.mounted.filter(n=>!after.mounted.includes(n)).length;
        trace.push({direction,before,after,...summarize(metrics)});
        assertMarkdownAnchor(before,after, 'Adjacent chunk load/unload moved the reading block');
        checkBounded(metrics,'large-markdown/scroll');
        index+=direction;
      }
      assert.ok(loads>0&&unloads>0, `Scroll did not exercise both loading and unloading: ${loads}/${unloads}`);
      await screenshot('large-markdown-scroll-anchor');
    });
    await check('large-markdown-resize-anchor', async () => {
      await loadMarkdown();
      await page.evaluate(() => window.test.search(2,'markdown-middle-6000'));
      await page.waitForFunction(() => window.test.highlights().some(hit=>hit.text==='markdown-middle-6000'));
      await settle();
      const initial=await markdownSnapshot(true);
      assert.ok(initial.reading?.chunk>0, 'Resize must start in a deep Markdown chunk');
      await scrollMarkdown(initial.reading.chunk,.75);
      await settle();
      const before=await markdownSnapshot(true),trace=[];
      assert.ok(before.reading.inset>800, 'Resize must exercise an internal block, not the chunk boundary');
      report.scenarios.push({name:'large-markdown-resize-anchor',before,trace});
      try {
        for (const width of [760,1280]) {
          await page.setViewportSize({width,height:720});
          await settle();
          const after=await markdownSnapshot(),metrics=await stats();
          trace.push({width,after,...summarize(metrics)});
          assertMarkdownAnchor(before,after,'Resize lost deep-chunk reading anchor');
          checkBounded(metrics,'large-markdown/resize');
        }
        await screenshot('large-markdown-resize-anchor');
      } finally { await page.setViewportSize({width:1280,height:800}); }
    });
    await check('large-markdown-stream-tail', async () => {
      await loadMarkdown();
      const initial=await stats();
      try {
        await page.evaluate(() => window.test.startStream());
        await page.waitForFunction(() => window.test.stats().streamTicks>=12);
        await waitBottom();
        const tail=await stats();
        report.scenarios.push({name:'large-markdown-stream-tail',...summarize(tail)});
        assert.ok(tail.max>initial.max,'Large Markdown streaming must grow the tail');
        checkBounded(tail,'large-markdown/stream-tail');
        await page.evaluate(() => window.test.search(2,'markdown-middle-6000'));
        await page.waitForFunction(() => window.test.highlights().some(hit=>hit.text==='markdown-middle-6000'));
        await settle();
        const before=await markdownSnapshot(true),browsing=await stats();
        await page.waitForFunction(n => window.test.stats().streamTicks>=n+12,browsing.streamTicks);
        const after=await markdownSnapshot(),streamed=await stats();
        report.scenarios.push({name:'large-markdown-stream-browse',before,after,...summarize(streamed)});
        assert.equal(streamed.stickToBottom,false,'Large Markdown tail stole free-browse mode');
        assertMarkdownAnchor(before,after,'Streaming tail moved the middle reading block');
        checkBounded(streamed,'large-markdown/stream-browse');
      } finally { await page.evaluate(() => window.test.stopStream()); }
      const completed=await stats();
      await page.evaluate(() => window.test.bottom());
      await waitText(`stream-${completed.streamTicks}`);
      await waitBottom();
      await settle();
      checkBounded(await stats(),'large-markdown/stream-complete');
      await screenshot('large-markdown-stream-tail');
    });

    await check('editing', async () => {
    // Exercise the actual editor without submitting a message or invoking the backend.
    await page.evaluate(() => window.test.load(30));
    await page.evaluate(() => window.test.jump(10));
    await waitText('round-10');
    await settle();
    await openEditor(10);
    const editor = page.locator('#fixture textarea');
    await editor.waitFor({ state: 'visible' });
    await editor.fill('edited 中文 😀 draft\nsecond line');
    assert.equal(await editor.inputValue(), 'edited 中文 😀 draft\nsecond line');
    assert.equal((await stats()).focused, true, 'hasFocusedInput must protect editing keyboard input');
    const editingTop = (await stats()).top;
    await editor.press('Home');
    await editor.press('ArrowUp');
    assert.ok(Math.abs((await stats()).top - editingTop) <= limits.anchorPx, 'Editor navigation scrolled the transcript');
    await screenshot('editing');
    await editor.press('Escape');
    await editor.waitFor({ state: 'hidden' });
    await waitText('round-10');
    });

    await check('inline-search', async () => {
      await page.evaluate(() => window.test.inline());
      await page.evaluate(() => window.test.search(2, 'alpha bold omega'));
      await page.waitForFunction(() => window.test.highlights().some(hit => hit.text==='alpha bold omega'));
      const hits=await page.evaluate(() => window.test.highlights());
      assert.ok(hits.some(hit=>hit.top>=0&&hit.top<700), 'Cross-inline highlight is offscreen');
      await screenshot('inline-search');
    });
    for (const kind of ['thought','tool']) await check(`deferred-search-${kind}`, async () => {
      await page.evaluate(kind => window.test.deferred(kind), kind);
      await waitText('answer-end-79');
      assert.equal((await page.evaluate(() => window.test.loadStats())).calls.length,0,'Offscreen deferred history was eagerly requested');
      const id=kind==='tool'?9002:9001, query=kind==='tool'?'deferred-tool-target':'deferred thought target';
      await page.evaluate(({id,query})=>window.test.search(id,query),{id,query});
      await page.waitForFunction(() => window.test.loadStats().pending>0);
      await settle();
      assert.ok((await page.evaluate(() => window.test.loadStats())).calls.flat().includes(id),'Target was not requested');
      await page.evaluate(() => window.test.releaseLoads());
      await page.waitForFunction(query=>window.test.highlights().some(hit=>hit.text===query),query);
      await settle();
      const hits=await page.evaluate(() => window.test.highlights());
      assert.ok(hits.some(hit=>hit.top>=0&&hit.top<700),'Hydrated match must scroll into view, not just highlight hidden content');
      assert.equal((await page.evaluate(() => window.test.loadStats())).hydrationCount,1);
      assert.equal((await stats()).active,30,'Hydration replaced the target reading group');
      await screenshot(`deferred-${kind}`);
    });
    await check('deferred-above-reading-anchor', async () => {
      await page.evaluate(() => {window.test.deferred('above');window.test.jump(30);});
      await page.waitForFunction(() => window.test.loadStats().pending>0);
      await page.evaluate(() => window.test.jump(31));
      await settle();
      const before=await page.evaluate(() => window.test.anchor());
      assert.equal(before?.index,31,'Fixture must browse the group after deferred content');
      await page.evaluate(() => window.test.releaseLoads());
      await page.waitForFunction(() => window.test.loadStats().hydrationCount===1);
      await settle();
      const after=await page.evaluate(() => window.test.anchor());
      assert.equal(after?.index,before.index,'Hydration above reading changed the active group');
      assert.ok(Math.abs(after.offset-before.offset)<=limits.anchorPx,`Hydration above shifted reading offset ${before.offset} -> ${after.offset}`);
      await screenshot('deferred-above-anchor');
    });
    await check('deferred-search-browse-away', async () => {
      await page.evaluate(() => {window.test.deferred('thought');window.test.search(9001,'deferred thought target');});
      await page.waitForFunction(() => window.test.loadStats().pending>0);
      await page.evaluate(() => window.test.jump(40));
      await settle();
      const before=await page.evaluate(() => window.test.anchor());
      await page.evaluate(() => window.test.releaseLoads());
      await settle();
      const after=await page.evaluate(() => window.test.anchor());
      assert.equal(after?.index,before?.index,'Resolved pending search stole later explicit navigation');
    });
    await check('deferred-late-session-switch', async () => {
      await page.evaluate(() => {window.test.deferred('tool');window.test.search(9002,'deferred-tool-target');});
      await page.waitForFunction(() => window.test.loadStats().pending>0);
      await page.evaluate(() => window.test.sameIds('replacement-deferred'));
      await page.evaluate(() => window.test.load(3));
      await page.evaluate(() => window.test.releaseLoads());
      await settle();
      assert.deepEqual(await page.evaluate(() => window.test.highlights()),[],'Late hydration resurrected an abandoned search');
      assert.equal(await page.locator('[data-item-id="9002"]').count(),0,'Abandoned deferred item leaked into new session');
    });
    await check('native-selection-new-session-helper', async () => {
      await page.evaluate(() => window.test.load(3));
      await page.evaluate(() => window.test.jump(1));
      await settle();
      const selected = await page.evaluate(() => {
        const element=document.querySelector('[data-group-index="1"] .ht-user-text');
        const range=document.createRange();range.selectNodeContents(element);
        const selection=window.getSelection();selection.removeAllRanges();selection.addRange(range);
        return {native:selection.toString(),helper:window.test.selectedChatText()};
      });
      assert.deepEqual(selected, { native: 'round-1', helper: 'round-1' }, 'New-session shortcut must receive the native chat selection');
      await screenshot('native-selection');
      await page.evaluate(() => window.test.sameIds('next-thread'));
      await settle();
      assert.equal(await page.evaluate(() => window.test.selectedChatText()), '', 'Old selection leaked into a new session');
    });
    await check('same-ids-edit-and-search', async () => {
      await page.evaluate(() => window.test.load(3));
      await page.evaluate(() => window.test.search(4, 'round-1'));
      await settle();
      await openEditor(1);
      await page.locator('#fixture textarea').fill('OLD SESSION DRAFT');
      await page.evaluate(() => window.test.sameIds('next-thread'));
      await settle();
      assert.equal(await page.locator('#fixture textarea').count(), 0, 'Same indices/item IDs reused old editing state');
      assert.deepEqual(await page.evaluate(() => window.test.highlights()), [], 'Search highlight leaked into new session');
      assert.equal(await page.locator('.transcript-search-hit').count(), 0, 'Old search-hit class leaked');
      await page.evaluate(() => window.test.jump(1));
      await settle();
      await openEditor(1);
      assert.equal(await page.locator('#fixture textarea').inputValue(), 'next-thread round-1', 'New editor resurrected old draft');
      await page.locator('#fixture textarea').press('Escape');
    });
    await check('same-ids-expanded', async () => {
      await page.evaluate(() => window.test.loadFold('paragraphs'));
      await page.evaluate(() => window.test.jump(4));
      await settle();
      await clickLabel('展开完整提示词');
      await page.waitForFunction(() => window.test.expanded(13));
      await page.evaluate(() => window.test.sameIds('next-thread'));
      await page.evaluate(() => window.test.jump(4));
      await settle();
      const fold=page.locator('[data-group-index="4"] button[aria-expanded]').filter({hasText:/提示词/}).first();
      assert.equal(await fold.getAttribute('aria-expanded'), 'false', 'Expanded state for reused item ID leaked across sessions');
    });
    assert.deepEqual(errors, [], 'Renderer emitted uncaught browser errors');
    report.passed = report.checks.every(check => check.passed || check.skipped);
  } catch (error) {
    report.lastStats = await stats().then(s => ({ ...summarize(s), top: s.top, max: s.max, active: s.active, bottom: s.bottom, streamTicks: s.streamTicks })).catch(() => null);
    report.lastText = await page.evaluate(() => window.test?.text().slice(0, 1200)).catch(() => null);
    await screenshot('failure').catch(() => {});
    throw error;
  } finally {
    report.pageErrors = errors;
    await context.close();
  }
}

let browser;
const reports = [];
try {
  const browserPath = buildOnly ? null : await executable();
  for (const report of [{ passed: false, scenarios: [] }]) {
    reports.push(report);
    let server;
    try {
      const built = await bundle();
      if (buildOnly) { report.passed = true; report.buildOnly = true; continue; }
      const files = new Map(built.outputFiles.map(file => ['/' + path.basename(file.path), file.contents]));
      server = createServer((req, res) => {
        const pathname = new URL(req.url, 'http://localhost').pathname;
        if (pathname === '/') {
          res.setHeader('Content-Type', 'text/html; charset=utf-8');
          res.end('<!doctype html><html data-theme="ink-dark"><head><meta charset="utf-8"><link rel="stylesheet" href="/bundle.css"><style>html,body,#root{margin:0;width:100%;height:100%;overflow:hidden}#fixture{height:calc(100vh - 100px);width:100%;display:flex;min-height:0}*{scroll-behavior:auto!important}</style></head><body><div id="root"></div><script type="module" src="/bundle.js"></script></body></html>');
        } else if (pathname.startsWith('/img-')) {
          // Delayed like an asset:// disk read, so the first switch to an image session is cold.
          setTimeout(() => { res.setHeader('Content-Type', 'image/svg+xml'); res.setHeader('Cache-Control', 'max-age=3600');
            res.end('<svg xmlns="http://www.w3.org/2000/svg" width="240" height="160"><rect width="240" height="160" fill="#41609a"/></svg>'); }, 40);
        } else if (files.has(pathname)) {
          res.setHeader('Content-Type', pathname.endsWith('.css') ? 'text/css' : pathname.endsWith('.js') ? 'text/javascript' : 'application/octet-stream');
          res.end(files.get(pathname));
        } else { res.statusCode = 404; res.end(); }
      });
      await new Promise((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve); });
      browser ??= await chromium.launch({ executablePath: browserPath, headless: true });
      await run(browser, server.address().port, report);
    } catch (error) {
      report.error = error.stack || String(error);
      console.error(report.error);
    } finally {
      if (server) await new Promise(resolve => server.close(resolve));
    }
  }
  const result = { buildOnly, source, selectedChecks, partialCoverage: selectedChecks.length > 0, browserPath, viewport: { width: 1280, height: 800, deviceScaleFactor: 1 }, limits, reports };
  await writeFile(path.join(output, 'results.json'), JSON.stringify(result, null, 2) + '\n');
  if (!buildOnly) {
    const images=(await readdir(output)).filter(name=>name.endsWith('.png')).sort();
    await writeFile(path.join(output,'index.html'),'<!doctype html><meta charset="utf-8"><title>Transcript screenshots</title><style>body{font:14px system-ui;background:#ddd}figure{margin:8px;display:inline-block;width:48%;vertical-align:top}img{width:100%}</style><h1>HtmlTranscript screenshots</h1><p>Read results.json for assertions and partial coverage.</p>'+images.map(n=>'<figure><figcaption>'+n+'</figcaption><img src="'+n+'"></figure>').join(''));
  }
  console.log(JSON.stringify(result, null, 2));
  console.log(`Artifacts: ${output}`);
  if (reports.some(report => !report.passed)) process.exitCode = 1;
} finally {
  await browser?.close();
  if (revision) await rm(sourceRoot, { recursive: true, force: true });
  if (buildOnly && !outputArg) await rm(output, { recursive: true, force: true });
}
