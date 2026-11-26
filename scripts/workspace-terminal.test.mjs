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
try {
  await writeFile(`${name}.html`,`<div id="root"></div><script type="module" src="/${name}.tsx"></script>`);
  await writeFile(`${name}.tsx`, `
import { render } from '@solidjs/web';
import { flush } from 'solid-js';
import { createSignal, Show } from 'solid-js';
import WorkspaceTerminal from './src/components/WorkspaceTerminal';
import { getTerminalGroup, terminalApi, closeTerminalTab } from './src/terminalSessions';
import { setState } from './src/store';
import './src/app.css';
let callback=0;
window.__TAURI_INTERNALS__={transformCallback:()=>++callback};
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
terminalApi.write=async(id,data)=>{writes.push({id,data});};
terminalApi.resize=async(id,cols,rows)=>{
  resized.push({id,cols,rows});
  if(gatedResize){gatedResize=false;await new Promise(resolve=>resizeGates.set(id,resolve));}
};
terminalApi.ack=async(id,bytes)=>{acknowledged.push({id,bytes});};
terminalApi.close=async id=>{closed.push(id);};
flush(() => setState(s => { Object.assign(s, {currentId:'a',cwd:'/project/a',theme:'ink-light',items:[]}); }));
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
 gateResize:()=>{gatedResize=true;},hasResizeGate:id=>resizeGates.has(id),
 releaseResize:id=>{resizeGates.get(id)?.();resizeGates.delete(id);},
 closeAll:async()=>{for(const id of ['a','b','err','gated','init','agy']){const g=getTerminalGroup('thread:'+id);for(const tab of g.tabs())await closeTerminalTab(g,tab);}},
};
function Fixture(){
 return <div style="display:flex;height:100vh;width:100vw"><main style="flex:1;min-width:0;display:flex"><Show when={open()}><Show when={thread()} keyed>{id=><WorkspaceTerminal threadId={id} cwd={'/project/'+id} initCommand={initCommand()} fontSize={fontSize()}/>}</Show></Show></main></div>;
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
  await page.goto(`http://127.0.0.1:${port}/${name}.html`,{waitUntil:'domcontentloaded',timeout:60000});
  await page.waitForFunction(()=>window.termTest?.status()==='running');
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
  if(process.env.TEST_SCREENSHOT)await page.screenshot({path:process.env.TEST_SCREENSHOT});
  await page.evaluate(()=>window.termTest.closeAll());
  await page.waitForFunction(()=>window.termTest.closed.length===window.termTest.created.length);
  assert.deepEqual(errors,[]);
  console.log('Workspace terminal: Nova eye-friendly light/dark themes, no horizontal drift, TUI shortcuts, stable OSC titles/tab DOM, footer status, UTF-8, control keys, tabs, hide/remount, wheel after restore, conversation retention, paste, spawn/close races, errors and resize passed');
} catch(error) { if(process.env.TEST_SCREENSHOT&&page)await page.screenshot({path:process.env.TEST_SCREENSHOT}).catch(()=>{}); throw error; } finally { await browser?.close();server?.kill();await Promise.all([`${name}.html`,`${name}.tsx`].map(path=>rm(path,{force:true}))); }
