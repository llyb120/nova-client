import assert from 'node:assert/strict';
import { access, writeFile, rm } from 'node:fs/promises';
import { spawn } from 'node:child_process';
import { chromium } from 'playwright-core';
const name=`workspace-terminal-check-${process.pid}`;
let server,browser,page;
async function assertTerminalTheme(expectedTheme) {
  const expected = expectedTheme === 'ink-light'
    ? { bg: '#eff3f9', fg: '#242d3d', cursor: '#365fe0', cssBg: 'rgb(239, 243, 249)' }
    : { bg: '#141821', fg: '#d6dce8', cursor: '#86a8ff', cssBg: 'rgb(20, 24, 33)' };
  await page.waitForFunction((bg) => {
    const tab = window.termTest.tab(window.termTest.id());
    return tab?.terminal?.options?.theme?.background === bg;
  }, expected.bg, { timeout: 3000 });
  const colors = await page.evaluate(() => {
    const tab = window.termTest.tab(window.termTest.id());
    const surfaceEl = document.querySelector('.workspace-terminal-surface');
    return {
      theme: tab.terminal.options.theme,
      screenReaderMode: tab.terminal.options.screenReaderMode,
      surface: getComputedStyle(surfaceEl).backgroundColor,
      filter: getComputedStyle(surfaceEl).filter,
      viewport: getComputedStyle(document.querySelector('.xterm-viewport')).backgroundColor,
    };
  });
  assert.equal(colors.screenReaderMode, false);
  assert.equal(colors.theme.background, expected.bg);
  assert.equal(colors.theme.foreground, expected.fg);
  assert.equal(colors.theme.cursor, expected.cursor);
  assert.equal(colors.surface, expected.cssBg);
  assert.equal(colors.viewport, expected.cssBg);
  assert.equal(colors.filter, 'none');
}
async function assertStableStatusDuringOutput(id) {
  const result=await page.evaluate(async id=>{
    const test=window.termTest,terminal=test.tab(id).terminal;
    const bar=document.querySelector('.workspace-terminal-status');
    const markup=bar.innerHTML,rect=bar.getBoundingClientRect(),activeId=test.id();
    let mutations=0,titleEvents=0;
    const observer=new MutationObserver(records=>{mutations+=records.length;});
    observer.observe(bar,{childList:true,characterData:true,attributes:true,subtree:true});
    // Prove real OSC 0/2 sequences reached xterm, rather than bypassing its parser.
    const listener=terminal.onTitleChange(()=>{titleEvents++;});
    try {
      for(let i=0;i<60;i++) {
        const end=i%2?'\x07':'\x1b\\';
        const data=new TextEncoder().encode(`\x1b]${i%2?0:2};C:\\Windows\\system32\\cmd.exe - progress ${i}${end}\x1b[32moutput ${i}\x1b[0m\r\n`);
        test.send(id,{type:'data',data:Array.from(data.subarray(0,3))});
        test.send(id,{type:'data',data:Array.from(data.subarray(3))});
      }
      await new Promise(resolve=>terminal.write('',resolve));
      mutations+=observer.takeRecords().length;
      const after=bar.getBoundingClientRect();
      return {mutations,titleEvents,unchanged:bar.innerHTML===markup,
        sameSize:rect.width===after.width&&rect.height===after.height,
        sameActive:test.id()===activeId};
    } finally { observer.disconnect();listener.dispose(); }
  },id);
  assert.equal(result.titleEvents,60,'OSC titles must be parsed by the real terminal');
  assert.equal(result.mutations,0,'output and shell titles must not mutate the status bar');
  assert.ok(result.unchanged&&result.sameSize&&result.sameActive);
}
async function assertScrollAfterRestore(id) {
  for (const wake of ['visibilitychange','focus']) {
    const result=await page.evaluate(async ({id,wake})=>{
      const {host,terminal}=window.termTest.tab(id);
      const frames=()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)));
      const resizeCount=window.termTest.resized.length;
      // Hide the terminal without resizing its panel, as a same-size restore can miss ResizeObserver.
      host.style.display='none';
      if(wake==='visibilitychange') {
        Object.defineProperty(document,'hidden',{configurable:true,value:true});
        document.dispatchEvent(new Event(wake));
      }
      await new Promise(resolve=>terminal.write('output while hidden\r\n'.repeat(80),resolve));
      await frames();
      const before=terminal.buffer.active.viewportY;
      host.style.display='';
      delete document.hidden;
      (wake==='visibilitychange'?document:window).dispatchEvent(new Event(wake));
      await frames();
      return {before,after:terminal.buffer.active.viewportY,resized:window.termTest.resized.length!==resizeCount};
    },{id,wake});
    assert.ok(result.before>0,'the terminal must have scrollback');
    assert.equal(result.after,result.before,`${wake} should preserve the scroll position`);
    assert.equal(result.resized,false,`${wake} should not resize the PTY at the same size`);
    await page.locator('.xterm-screen').hover();
    await page.mouse.wheel(0,-120);
    await page.waitForFunction(({id,y})=>window.termTest.tab(id).terminal.buffer.active.viewportY<y,
      {id,y:result.after},{timeout:3000});
  }
}
async function assertConptyRepaintPreservesHistory(id) {
  const {expected,actual}=await page.evaluate(async id=>{
    const test=window.termTest,{terminal}=test.tab(id),{cols,rows}=terminal;
    const send=async text=>{
      test.send(id,{type:'data',data:Array.from(new TextEncoder().encode(text))});
      await new Promise(resolve=>terminal.write('',resolve));
    };
    terminal.reset();
    const expected=Array.from({length:rows+4},(_,i)=>`ConPTY history ${i+1}`);
    await send(expected.join('\r\n'));
    try {
      terminal.resize(cols,rows+4);
      // On resize ConPTY repaints the old viewport from home, not the scrollback before it.
      await send('\x1b[H'+expected.slice(-rows).map(line=>`${line}\x1b[K`).join('\r\n')+'\x1b[J');
      const buffer=terminal.buffer.active;
      const actual=Array.from({length:buffer.length},(_,i)=>buffer.getLine(i)?.translateToString(true)).filter(Boolean);
      return {expected,actual};
    } finally { terminal.resize(cols,rows); }
  },id);
  assert.deepEqual(actual,expected,'ConPTY resize repaint must retain history without duplicating lines');
}
async function assertScrollDuringResize(id) {
  const original=page.viewportSize();
  const before=await page.evaluate(async id=>{
    const test=window.termTest,{terminal}=test.tab(id);
    terminal.reset();
    const lines=Array.from({length:terminal.rows+80},(_,i)=>`Antigravity history ${i+1}`);
    test.send(id,{type:'data',data:Array.from(new TextEncoder().encode(lines.join('\r\n')))});
    await new Promise(resolve=>terminal.write('',resolve));
    await new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)));
    terminal.scrollToLine(8);
    return {cols:terminal.cols,rows:terminal.rows};
  },id);
  const resize=async (viewport, comparison)=>{
    const since=await page.evaluate(()=>window.termTest.resized.length);
    await page.setViewportSize(viewport);
    await page.waitForFunction(({id,since,before,comparison})=>
      window.termTest.resized.slice(since).some(resize=>resize.id===id&&
        (comparison==='smaller'?resize.cols<before.cols&&resize.rows<before.rows:
          resize.cols>=before.cols&&resize.rows>=before.rows)),{id,since,before,comparison});
  };
  const view=()=>page.evaluate(async id=>{
    const {terminal}=window.termTest.tab(id);
    await new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)));
    const buffer=terminal.buffer.active;
    const viewport=document.querySelector('.xterm-viewport');
    return {
      top:buffer.getLine(buffer.viewportY)?.translateToString(true),
      viewportY:buffer.viewportY,baseY:buffer.baseY,scrollTop:viewport.scrollTop,
      maxScroll:viewport.scrollHeight-viewport.clientHeight,
    };
  },id);
  const history=await view();
  assert.equal(history.top,'Antigravity history 9');
  await resize({width:original.width-240,height:original.height-180},'smaller');
  const smaller=await view();
  assert.equal(smaller.top,history.top,'shrinking the window must keep the visible history line');
  assert.equal(smaller.scrollTop,history.scrollTop,'the scrollbar should stay on the same line');
  // ConPTY repaints the live screen after the resize, while the user is still reading old output.
  await page.evaluate(async id=>{
    const test=window.termTest,{terminal}=test.tab(id),buffer=terminal.buffer.active;
    const live=Array.from({length:terminal.rows},(_,i)=>buffer.getLine(buffer.baseY+i)?.translateToString(true)??'');
    test.send(id,{type:'data',data:Array.from(new TextEncoder().encode('\x1b[H'+live.join('\x1b[K\r\n')+'\x1b[K\x1b[J'))});
    await new Promise(resolve=>terminal.write('',resolve));
  },id);
  assert.equal((await view()).top,history.top,'ConPTY repaint after resize must not move the reading position');
  await resize(original,'restored');
  assert.equal((await view()).top,history.top,'growing the window must keep the visible history line');

  await page.evaluate(id=>window.termTest.tab(id).terminal.scrollToBottom(),id);
  await resize({width:original.width-240,height:original.height-180},'smaller');
  const assertAtBottom=async()=>{
    const bottom=await view();
    assert.equal(bottom.viewportY,bottom.baseY,'keep following the end when the user was at the bottom');
    assert.ok(Math.abs(bottom.scrollTop-bottom.maxScroll)<1);
  };
  await assertAtBottom();
  await resize(original,'restored');
  await assertAtBottom();
}
async function assertImeAndInputGuards(id) {
  const original = page.viewportSize();
  // 1. Defer fitTerminal while IME composition is active, then flush after compositionend without dropping/duplicating text.
  const before = await page.evaluate(() => ({
    writes: window.termTest.writes.length,
    resizes: window.termTest.resized.length,
  }));
  await page.locator('.xterm-helper-textarea').focus();
  await page.evaluate(() => {
    const ta = document.querySelector('.xterm-helper-textarea');
    ta.value = '';
    ta.dispatchEvent(new CompositionEvent('compositionstart', { bubbles: true, data: '' }));
  });
  assert.equal(await page.evaluate(tid => window.termTest.tab(tid).composing, id), true);
  await page.setViewportSize({ width: original.width - 160, height: original.height - 120 });
  await page.evaluate(() => new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r))));
  assert.equal(await page.evaluate(() => window.termTest.resized.length), before.resizes, 'PTY resize must wait until IME composition ends');
  await page.evaluate(() => {
    const ta = document.querySelector('.xterm-helper-textarea');
    ta.value = '你';
    ta.dispatchEvent(new CompositionEvent('compositionupdate', { bubbles: true, data: '你' }));
  });
  await page.evaluate(() => new Promise(r => setTimeout(r, 10)));
  await page.evaluate(() => {
    const ta = document.querySelector('.xterm-helper-textarea');
    ta.dispatchEvent(new CompositionEvent('compositionend', { bubbles: true, data: '你' }));
  });
  await page.waitForFunction(resizes => window.termTest.resized.length > resizes, before.resizes);
  await page.waitForFunction(({ since, tid }) => {
    const bytes = window.termTest.writes.slice(since).filter(w => w.id === tid).flatMap(w => w.data);
    return new TextDecoder().decode(Uint8Array.from(bytes)).includes('你');
  }, { since: before.writes, tid: id });
  await page.evaluate(() => new Promise(r => setTimeout(r, 30)));
  const composedText = await page.evaluate(({ since, tid }) => {
    const bytes = window.termTest.writes.slice(since).filter(w => w.id === tid).flatMap(w => w.data);
    return new TextDecoder().decode(Uint8Array.from(bytes));
  }, { since: before.writes, tid: id });
  assert.equal(composedText, '你', 'compositionend must deliver the first CJK character once');
  await page.setViewportSize(original);
  await page.waitForFunction(resizes => window.termTest.resized.length > resizes + 1, before.resizes);

  // 2. Rescue a dropped IME commit when xterm CompositionHelper sees an empty textarea on compositionend.
  const beforeRescue = await page.evaluate(() => window.termTest.writes.length);
  await page.evaluate(() => {
    const ta = document.querySelector('.xterm-helper-textarea');
    ta.value = '';
    ta.dispatchEvent(new CompositionEvent('compositionstart', { bubbles: true, data: '' }));
    ta.dispatchEvent(new CompositionEvent('compositionend', { bubbles: true, data: '好' }));
  });
  await page.waitForFunction(({ since, tid }) => {
    const bytes = window.termTest.writes.slice(since).filter(w => w.id === tid).flatMap(w => w.data);
    return new TextDecoder().decode(Uint8Array.from(bytes)).includes('好');
  }, { since: beforeRescue, tid: id });
  await page.evaluate(() => new Promise(r => setTimeout(r, 30)));
  const rescuedText = await page.evaluate(({ since, tid }) => {
    const bytes = window.termTest.writes.slice(since).filter(w => w.id === tid).flatMap(w => w.data);
    return new TextDecoder().decode(Uint8Array.from(bytes));
  }, { since: beforeRescue, tid: id });
  assert.equal(rescuedText, '好', 'dropped IME compositionend commit must be rescued exactly once');

  // 3. Cancelled composition (empty data) must not emit anything.
  const beforeCancelled = await page.evaluate(() => window.termTest.writes.length);
  await page.evaluate(() => {
    const ta = document.querySelector('.xterm-helper-textarea');
    ta.value = '';
    ta.dispatchEvent(new CompositionEvent('compositionstart', { bubbles: true, data: '' }));
    ta.dispatchEvent(new CompositionEvent('compositionend', { bubbles: true, data: '' }));
  });
  await page.evaluate(() => new Promise(r => setTimeout(r, 40)));
  assert.equal(await page.evaluate(() => window.termTest.writes.length), beforeCancelled);

  // 4. sendTerminalInput immediately after sendTerminalCancel must wait past the ESC cooldown before sending first char.
  const beforeEsc = await page.evaluate(() => window.termTest.writes.length);
  const escResult = await page.evaluate(async () => {
    const cancelled = window.termTest.sendCancel();
    const sent = await window.termTest.sendInput('测试首字', { submit: true });
    return { cancelled, sent };
  });
  assert.deepEqual(escResult, { cancelled: true, sent: true });
  const escWrites = await page.evaluate(({ since, tid }) =>
    window.termTest.writes.slice(since).filter(w => w.id === tid),
    { since: beforeEsc, tid: id },
  );
  assert.deepEqual(escWrites[0].data, [27]);
  const textWrite = escWrites.find(w => new TextDecoder().decode(Uint8Array.from(w.data)) === '测试首字');
  assert.ok(textWrite, 'full prompt including first character must be written');
  assert.ok(textWrite.at - escWrites[0].at >= 120, `expected >=120ms ESC guard delay, got ${textWrite.at - escWrites[0].at}ms`);
  assert.deepEqual(escWrites.at(-1).data, [13]);
}
async function assertAgyWorldlineLayout() {
  await page.evaluate(()=>{
    localStorage.setItem('nova:agy-split-open','false'); // An old close preference must not hide a newly opened worldline.
    window.termTest.switch('agy-layout');
  });
  const worldline=page.getByRole('region',{name:'反重力 CLI 对话式世界线'});
  const terminal=page.getByRole('region',{name:'交互式终端'});
  const input=page.locator('.terminal-composer-input');
  const divider=page.getByRole('separator',{name:'调整 CLI 与世界线分屏比例'});
  await worldline.waitFor();
  await page.waitForFunction(()=>window.termTest.status()==='running');
  assert.ok(await input.evaluate(async field=>{
    await new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)));
    return document.activeElement===field;
  }),'the Nova composer should keep focus after the CLI mounts');
  const left=await worldline.boundingBox(),right=await terminal.boundingBox();
  const dock=await worldline.locator('.terminal-composer-dock').boundingBox();
  assert.ok(left.x+left.width<=right.x,'worldline must be left of the raw CLI');
  assert.ok(Math.abs(left.width/(left.width+right.width)-2/3)<0.01,'the worldline should start at a 2:1 split');
  assert.equal(dock.width,left.width,'the composer must not span both panes');
  assert.ok(dock.y>left.y&&Math.abs(dock.y+dock.height-right.y-right.height)<1);
  assert.ok((await input.boundingBox()).height>=76,'the AGY composer should start with more writing room');

  const handle=await divider.boundingBox();
  const x=handle.x+handle.width/2,y=handle.y+handle.height/2;
  await page.mouse.move(x,y); await page.mouse.down(); await page.mouse.move(x-80,y);
  const guide=await page.locator('.terminal-split-guide').boundingBox();
  assert.ok(Math.abs(guide.x-(x-80))<2,'preview should track the dragged boundary');
  assert.equal((await worldline.boundingBox()).width,left.width,'dragging must only preview the new split');
  await page.mouse.up();
  assert.ok((await worldline.boundingBox()).width<left.width-60,'dragging left must narrow the worldline');
  await divider.dblclick();
  assert.ok(Math.abs((await worldline.boundingBox()).width-left.width)<1);

  const session=await page.evaluate(()=>window.termTest.id());
  await input.fill('保留这个草稿');
  await worldline.getByRole('button',{name:'收起世界线分屏'}).click();
  await worldline.waitFor({state:'hidden'});
  assert.ok((await terminal.boundingBox()).width>right.width,'the CLI should expand when the worldline is hidden');
  assert.equal(await input.inputValue(),'保留这个草稿');
  assert.equal(await page.evaluate(()=>window.termTest.id()),session,'closing the worldline must not restart the terminal');
  await page.evaluate(()=>window.termTest.toggle());
  await terminal.waitFor({state:'hidden'});
  await page.evaluate(()=>window.termTest.toggle());
  await worldline.waitFor();
  assert.equal(await input.inputValue(),'保留这个草稿');
}
try {
  await writeFile(`${name}.html`,`<div id="root"></div><script type="module" src="/${name}.tsx"></script>`);
  await writeFile(`${name}.tsx`, `
import { render } from '@solidjs/web';
import { flush } from 'solid-js';
import { createSignal, Show } from 'solid-js';
import WorkspaceTerminal from './src/components/WorkspaceTerminal';
import TerminalSessionView from './src/components/TerminalSessionView';
import { getTerminalGroup, terminalApi, closeTerminalTab, sendTerminalCancel, sendTerminalInput } from './src/terminalSessions';
import { api } from './src/ipc';
import { setState } from './src/store';
import './src/app.css';
const created=[],closed=[],writes=[],resized=[],acknowledged=[];
const channels=new Map(),pending=new Map();
let deferred=false,fail=false,gatedResize=false,rewrittenInit=null;
const resizeGates=new Map();
terminalApi.create=async (id,threadId,cwd,cols,rows,channel,initCommand,allowResume)=>{
  created.push({id,threadId,cwd,cols,rows,initCommand:initCommand??null,allowResume:!!allowResume});channels.set(id,channel);
  if(created.length===1)channel.onmessage({type:'windowsPty',build:19045});
  if(fail){fail=false;throw Error('test: shell not found');}
  if(deferred){deferred=false;await new Promise(resolve=>pending.set(id,resolve));}
  if(rewrittenInit!==null){const next=rewrittenInit;rewrittenInit=null;return {initCommand:next};}
};
terminalApi.write=async(id,data)=>{writes.push({id,data,at:performance.now()});};
terminalApi.resize=async(id,cols,rows)=>{
  resized.push({id,cols,rows});
  if(gatedResize){gatedResize=false;await new Promise(resolve=>resizeGates.set(id,resolve));}
};
terminalApi.ack=async(id,bytes)=>{acknowledged.push({id,bytes});};
terminalApi.close=async id=>{closed.push(id);};
api.getAgyTerminalTurns=async()=>[];
flush(() => setState(s => {
  Object.assign(s, {currentId:'a',cwd:'/project/a',theme:'ink-light',items:[]});
  s.threads.push({id:'agy-layout',title:'Antigravity',cwd:'/project/agy-layout',agentKind:'lyra',
    createdAt:1,updatedAt:1,running:false,starred:false,unreadTurns:0,terminalThread:true,
    terminalConfig:{mode:'antigravity',agyAccountId:'fixture-account',initCommand:'',shell:'',args:[]}});
}));
const [thread,setThread]=createSignal('a');
const [open,setOpen]=createSignal(true);
const [fontSize,setFontSize]=createSignal(13);
const [initCommand,setInitCommand]=createSignal('');
const group=()=>getTerminalGroup('thread:'+thread());
const active=()=>group().tabs().find(tab=>tab.id===group().activeId());
window.termTest={created,closed,writes,resized,acknowledged,
 id:()=>active()?.id,status:()=>active()?.status(),tab:id=>group().tabs().find(tab=>tab.id===id),
 setTheme:theme=>{setState(s => { s.theme = theme; });document.documentElement.dataset.theme=theme;},
 buffer:()=>{const b=active().terminal.buffer.active;return Array.from({length:b.length},(_,i)=>b.getLine(i)?.translateToString(true)??'').join('\\n');},
 send:(id,event)=>channels.get(id).onmessage(event),
 switch:id=>{setState(s => { Object.assign(s, {currentId:id,cwd:'/project/'+id}); });setThread(id);},
 toggle:()=>setOpen(v=>!v),
 setFontSize,
 setInitCommand,
 rewriteInit:cmd=>{rewrittenInit=cmd;},
 defer:()=>{deferred=true;},fail:()=>{fail=true;},resolve:id=>{pending.get(id)();pending.delete(id);},
 paste:text=>active().terminal.paste(text),
 sendInput:(text,options)=>sendTerminalInput(active(),text,options),
 sendCancel:()=>sendTerminalCancel(active()),
 gateResize:()=>{gatedResize=true;},hasResizeGate:id=>resizeGates.has(id),
 releaseResize:id=>{resizeGates.get(id)?.();resizeGates.delete(id);},
 closeAll:async()=>{for(const id of ['a','b','err','gated','init','agy','agy-layout']){const g=getTerminalGroup('thread:'+id);for(const tab of g.tabs())await closeTerminalTab(g,tab);}},
};
function Fixture(){
 return <div style="display:flex;height:100vh;width:100vw"><div style="flex:1;min-width:0;display:flex"><Show when={open()}><Show when={thread()} keyed>{id=>id==='agy-layout'
   ? <TerminalSessionView threadId={id}/>
   : <WorkspaceTerminal threadId={id} cwd={'/project/'+id} initCommand={initCommand()} fontSize={fontSize()}/>}</Show></Show></div></div>;
}
render(()=><Fixture/>,document.getElementById('root')!);
`);
  const port=15000+process.pid%1000;
  server=spawn(process.execPath,['node_modules/vite/bin/vite.js','--host','127.0.0.1','--port',String(port),'--strictPort'],{windowsHide:true,stdio:['ignore','pipe','pipe'],env:{...process.env,NO_COLOR:'1'}});
  await new Promise((resolve,reject)=>{const timeout=setTimeout(()=>reject(Error('Vite startup timed out')),30000);server.stdout.on('data',data=>{if(data.toString().includes('Local:')){clearTimeout(timeout);resolve();}});server.stderr.on('data',data=>process.stderr.write(data));server.on('exit',code=>{clearTimeout(timeout);reject(Error('Vite exited: '+code));});});
  let executablePath;
  for(const path of [process.env.TEST_BROWSER,'/usr/bin/google-chrome','/usr/bin/chromium','C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe'].filter(Boolean)){try{await access(path);executablePath=path;break;}catch{}}
  browser=await chromium.launch({executablePath,headless:true,args:['--no-sandbox']});
  page=await browser.newPage({viewport:{width:1280,height:800}});
  const errors=[];page.on('pageerror',error=>{errors.push(error.message);console.error(error.message);});
  page.setDefaultTimeout(15000);
  await page.addInitScript(()=>{
    let callback=0;
    window.__TAURI_INTERNALS__={
      transformCallback:()=>++callback,
      metadata:{currentWindow:{label:'main'},currentWebview:{label:'main'}},
    };
  });
  await page.goto(`http://127.0.0.1:${port}/${name}.html`,{waitUntil:'domcontentloaded',timeout:60000});
  await page.waitForFunction(()=>window.termTest?.status()==='running');
  await page.waitForFunction(()=>document.activeElement?.classList.contains('xterm-helper-textarea'));
  const first=await page.evaluate(()=>window.termTest.id());
  assert.equal(await page.evaluate(()=>window.termTest.created.length),1);
  assert.equal(await page.evaluate(()=>window.termTest.created[0].cwd),'/project/a');
  assert.ok((await page.getByRole('tabpanel').boundingBox()).height>500);
  for(const theme of ['ink-light','ink-dark','ink-light']) {
    await page.evaluate(theme=>window.termTest.setTheme(theme),theme);
    await assertTerminalTheme(theme);
  }
  assert.equal(await page.locator('.workspace-terminal [role=tab]').count(),0);
  assert.equal(await page.getByRole('button',{name:'新建终端'}).count(),0);
  assert.equal(await page.locator('.workspace-terminal-state').textContent(),'运行中');
  await assertStableStatusDuringOutput(first);
  await assertScrollAfterRestore(first);
  assert.equal(await page.evaluate(id=>{
    const {created,resized}=window.termTest;
    const spawned=created.find(s=>s.id===id),initialResize=resized.find(s=>s.id===id);
    return initialResize?.cols===spawned.cols&&initialResize.rows===spawned.rows;
  },first),false,'startup must not resize the PTY to the dimensions it already has');
  await assertConptyRepaintPreservesHistory(first);
  await assertScrollDuringResize(first);
  await page.getByRole('button',{name:'清空终端显示',exact:true}).click();
  await page.evaluate(id=>{for(const byte of new TextEncoder().encode('中文🙂 output\r\n'))window.termTest.send(id,{type:'data',data:[byte]});},first);
  await page.waitForFunction(()=>window.termTest.buffer().includes('中文🙂 output'));
  await page.waitForFunction(()=>window.termTest.acknowledged.length>5);

  // xterm must respond to ConPTY DSR through the same IPC input queue.
  const beforeQuery=await page.evaluate(()=>window.termTest.writes.length);
  await page.evaluate(id=>window.termTest.send(id,{type:'data',data:[27,91,54,110]}),first);
  await page.waitForFunction(before=>/\x1b\[\d+;\d+R/.test(String.fromCharCode(...window.termTest.writes.slice(before).flatMap(w=>w.data))),beforeQuery);

  await page.locator('.xterm-helper-textarea').focus();
  await page.keyboard.press('Escape');await page.keyboard.press('Control+c');await page.keyboard.press('Control+s');
  await page.waitForFunction(()=>[3,19,27].every(byte=>window.termTest.writes.some(write=>write.data.includes(byte))));
  // Verify TUI editing shortcuts: Shift+Enter -> ESC+CR, Ctrl+Backspace -> Ctrl+W, and no horizontal scroll drift.
  const beforeTuiKeys=await page.evaluate(()=>window.termTest.writes.length);
  await page.keyboard.press('Shift+Enter');
  await page.keyboard.press('Control+Backspace');
  await page.waitForFunction(before=>{
    const bytes=window.termTest.writes.slice(before).flatMap(w=>w.data);
    const hasShiftEnter=bytes.some((b,i)=>b===27&&bytes[i+1]===13);
    const hasCtrlW=bytes.includes(23);
    return hasShiftEnter&&hasCtrlW;
  },beforeTuiKeys);
  await page.evaluate(id=>{
    const box='╭'+'─'.repeat(160)+'╮\r\n│ 中文混排宽字符 TUI 测试 ' + 'x'.repeat(140) + ' │\r\n';
    window.termTest.send(id,{type:'data',data:Array.from(new TextEncoder().encode(box))});
  },first);
  await page.waitForFunction(()=>window.termTest.buffer().includes('中文混排宽字符 TUI 测试'));
  const scrollState=await page.evaluate(()=>({
    surface:document.querySelector('.workspace-terminal-surface')?.scrollLeft??-1,
    instance:document.querySelector('.workspace-terminal-instance')?.scrollLeft??-1,
    viewport:document.querySelector('.xterm-viewport')?.scrollLeft??-1,
  }));
  assert.deepEqual(scrollState,{surface:0,instance:0,viewport:0},'wide TUI output must never scroll horizontally');
  await assertImeAndInputGuards(first);
  await page.evaluate(()=>window.termTest.toggle());
  await page.waitForFunction(()=>!document.querySelector('.workspace-terminal'));
  assert.deepEqual(await page.evaluate(()=>window.termTest.closed),[]);
  await page.evaluate(()=>window.termTest.toggle());
  await page.waitForFunction(()=>!!document.querySelector('.xterm-helper-textarea'));
  assert.equal(await page.evaluate(()=>window.termTest.created.length),1);
  assert.match(await page.evaluate(()=>window.termTest.buffer()),/中文🙂 output/);
  await assertTerminalTheme('ink-light');
  assert.equal(await page.locator('.workspace-terminal [role=tab]').count(),0);
  await page.evaluate(()=>window.termTest.switch('b'));
  await page.waitForFunction(()=>window.termTest.created.length===2&&window.termTest.status()==='running');
  await page.evaluate(()=>window.termTest.switch('a'));
  await page.waitForFunction(id=>window.termTest.id()===id,first);
  await page.evaluate(()=>window.termTest.setTheme('ink-light'));
  await assertTerminalTheme('ink-light');
  assert.deepEqual(await page.evaluate(()=>window.termTest.closed),[]);
  await page.evaluate(()=>window.termTest.paste('x'.repeat(70000)));
  await page.waitForFunction(()=>window.termTest.writes.reduce((n,w)=>n+w.data.filter(b=>b===120).length,0)===70000);
  assert.ok(await page.evaluate(()=>window.termTest.writes.every(w=>w.data.length<=32768)));
  const beforeResize=await page.evaluate(()=>{
    const test=window.termTest,id=test.id(),{cols,rows}=test.tab(id).terminal;
    return {count:test.resized.length,id,cols,rows};
  });
  await page.setViewportSize({width:1000,height:640});
  await page.waitForFunction(before=>window.termTest.resized.slice(before.count).some(s=>s.id===before.id&&(s.cols!==before.cols||s.rows!==before.rows)),beforeResize);
  assert.ok(await page.evaluate(()=>window.termTest.resized.every(s=>s.cols>0&&s.rows>0)));
  // Exited terminal shows 重启 button and recovers cleanly into a new session PTY.
  await page.evaluate(()=>window.termTest.switch('b'));
  await page.waitForFunction(()=>window.termTest.id()!==undefined&&window.termTest.status()==='running');
  const bId=await page.evaluate(()=>window.termTest.id());
  await page.evaluate(id=>window.termTest.send(id,{type:'exit',code:0}),bId);
  await page.waitForFunction(()=>window.termTest.status()==='exited');
  assert.equal(await page.locator('.workspace-terminal-state').textContent(),'已退出');
  await page.getByRole('button',{name:'重启终端',exact:true}).click();
  await page.waitForFunction(id=>window.termTest.closed.includes(id)&&window.termTest.status()==='running',bId);
  // Startup error shows alert + 启动失败 and recovers on 重启终端.
  await page.evaluate(()=>{window.termTest.fail();window.termTest.switch('err');});
  await page.getByRole('alert').getByText('test: shell not found',{exact:false}).waitFor();
  assert.equal(await page.evaluate(()=>window.termTest.status()),'error');
  assert.equal(await page.locator('.workspace-terminal-state').textContent(),'启动失败');
  await page.getByRole('button',{name:'重启终端',exact:true}).click();
  await page.waitForFunction(()=>window.termTest.status()==='running');
  // Fit through several layouts while the PTY is starting and while a native resize is blocked.
  await page.evaluate(()=>{window.termTest.defer();window.termTest.switch('gated');});
  await page.waitForFunction(()=>window.termTest.created.at(-1)?.threadId==='gated');
  const {id:gated,rows:spawnRows}=await page.evaluate(()=>window.termTest.created.at(-1));
  assert.equal(await page.locator('.workspace-terminal-state').textContent(),'启动中');
  const gatedResizes=()=>page.evaluate(id=>window.termTest.resized.filter(s=>s.id===id),gated);
  const setHeight=height=>page.evaluate(async ({id,height})=>{
    const {host,terminal}=window.termTest.tab(id);
    host.style.height=height;
    await new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));
    return {id,cols:terminal.cols,rows:terminal.rows};
  },{id:gated,height});
  const beforeStart=await setHeight('80%');
  const afterStart=await setHeight('60%');
  assert.ok(spawnRows>beforeStart.rows&&beforeStart.rows>afterStart.rows);
  assert.deepEqual(await gatedResizes(),[]);
  await page.evaluate(id=>{window.termTest.gateResize();window.termTest.resolve(id);},gated);
  await page.waitForFunction(id=>window.termTest.hasResizeGate(id),gated);
  assert.deepEqual(await gatedResizes(),[afterStart],'startup should send only the latest dimensions');

  const intermediate=await setHeight('70%');
  const latest=await setHeight('90%');
  assert.ok(afterStart.rows<intermediate.rows&&intermediate.rows<latest.rows);
  assert.deepEqual(await gatedResizes(),[afterStart],'native resizes must not overlap');
  const beforeGatedQuery=await page.evaluate(()=>window.termTest.writes.length);
  await page.evaluate(id=>window.termTest.send(id,{type:'data',data:[27,91,54,110]}),gated);
  await page.waitForFunction(before=>window.termTest.writes.slice(before).some(w=>w.data.at(-1)===82),beforeGatedQuery);
  await page.evaluate(id=>{window.termTest.gateResize();window.termTest.releaseResize(id);},gated);
  await page.waitForFunction(id=>window.termTest.hasResizeGate(id),gated);
  assert.deepEqual(await gatedResizes(),[afterStart,latest],'after a blocked resize only the final dimensions matter');
  await page.evaluate(id=>window.termTest.releaseResize(id),gated);
  // Test initCommand after ConPTY DSR handshake and fontSize updates.
  await page.evaluate(()=>{
    window.termTest.setFontSize(16);
    window.termTest.setInitCommand('echo hello\r\npwd');
    window.termTest.switch('init');
  });
  await page.waitForFunction(()=>window.termTest.created.at(-1)?.threadId==='init'&&window.termTest.status()==='running');
  const initTabId=await page.evaluate(()=>window.termTest.id());
  assert.equal(await page.evaluate(id=>window.termTest.tab(id).terminal.options.fontSize,initTabId),16);
  const beforeInit=await page.evaluate(()=>window.termTest.writes.length);
  await page.evaluate(id=>window.termTest.send(id,{type:'data',data:[27,91,54,110]}),initTabId);
  await page.waitForFunction(({before,id})=>{
    const text=String.fromCharCode(...window.termTest.writes.slice(before).filter(w=>w.id===id).flatMap(w=>w.data));
    return /\x1b\[\d+;\d+R/.test(text)&&text.includes('echo hello\rpwd\r')&&text.indexOf('echo hello\rpwd\r')>text.indexOf('R');
  },{before:beforeInit,id:initTabId});
  // Test backend-rewritten agy initCommand and allowResume flag propagation.
  await page.evaluate(()=>{
    window.termTest.setInitCommand('agy');
    window.termTest.rewriteInit('agy --resume conv-1 --log-file "%NOVA_AGY_LOG_FILE%"');
    window.termTest.switch('agy');
  });
  await page.waitForFunction(()=>window.termTest.created.at(-1)?.threadId==='agy'&&window.termTest.status()==='running');
  const agyCreated=await page.evaluate(()=>window.termTest.created.at(-1));
  assert.equal(agyCreated.initCommand,'agy');
  assert.equal(agyCreated.allowResume,true);
  await page.waitForFunction(id=>String.fromCharCode(...window.termTest.writes.filter(w=>w.id===id).flatMap(w=>w.data)).includes('agy --resume conv-1 --log-file "%NOVA_AGY_LOG_FILE%"\r'),agyCreated.id);
  await assertAgyWorldlineLayout();
  if(process.env.TEST_SCREENSHOT)await page.screenshot({path:process.env.TEST_SCREENSHOT});
  await page.evaluate(()=>window.termTest.closeAll());
  await page.waitForFunction(()=>window.termTest.closed.length===window.termTest.created.length);
  assert.deepEqual(errors,[]);
  console.log('Workspace terminal: Nova eye-friendly light/dark themes, no horizontal drift, TUI shortcuts, stable OSC titles/tab DOM, footer status, UTF-8, control keys, tabs, hide/remount, wheel after restore, stable scroll on resize, conversation retention, paste, spawn/close races, errors and AGY split layout passed');
} catch(error) { if(process.env.TEST_SCREENSHOT&&page)await page.screenshot({path:process.env.TEST_SCREENSHOT}).catch(()=>{}); throw error; } finally { await browser?.close();server?.kill();await Promise.all([`${name}.html`,`${name}.tsx`].map(path=>rm(path,{force:true}))); }
