import assert from 'node:assert/strict';
import { access, writeFile, rm } from 'node:fs/promises';
import { spawn } from 'node:child_process';
import { chromium } from 'playwright-core';
const name=`workspace-terminal-check-${process.pid}`;
let server,browser,page;
async function assertBlackTerminal() {
  const colors=await page.evaluate(()=>{
    const tab=window.termTest.tab(window.termTest.id());
    return {
      theme:tab.terminal.options.theme,
      surface:getComputedStyle(document.querySelector('.workspace-terminal-surface')).backgroundColor,
      viewport:getComputedStyle(document.querySelector('.xterm-viewport')).backgroundColor,
    };
  });
  assert.equal(colors.theme.background,'#000000');
  assert.equal(colors.theme.foreground,'#d4d8df');
  assert.equal(colors.theme.cursor,'#d4d8df');
  assert.equal(colors.surface,'rgb(0, 0, 0)');
  assert.equal(colors.viewport,'rgb(0, 0, 0)');
}
async function assertStableTabsDuringOutput(id) {
  const result=await page.evaluate(async id=>{
    const test=window.termTest,terminal=test.tab(id).terminal;
    const bar=document.querySelector('[aria-label="终端标签页"]');
    const button=bar.querySelector('[role=tab]');
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
        sameNode:bar.querySelector('[role=tab]')===button,
        sameSize:rect.width===after.width&&rect.height===after.height,
        sameActive:test.id()===activeId};
    } finally { observer.disconnect();listener.dispose(); }
  },id);
  assert.equal(result.titleEvents,60,'OSC titles must be parsed by the real terminal');
  assert.equal(result.mutations,0,'output and shell titles must not mutate the tab bar');
  assert.ok(result.unchanged&&result.sameNode&&result.sameSize&&result.sameActive);
}
try {
  await writeFile(`${name}.html`,`<div id="root"></div><script type="module" src="/${name}.tsx"></script>`);
  await writeFile(`${name}.tsx`, `
import { render } from 'solid-js/web';
import { createSignal, Show } from 'solid-js';
import WorkspacePanel from './src/components/WorkspacePanel';
import { getTerminalGroup, terminalApi, closeTerminalTab } from './src/terminalSessions';
import { setState } from './src/store';
import { api } from './src/ipc';
import { mountSessionShortcuts } from './src/sessionShortcuts';
import { workspaceLayout, setWorkspaceLayout } from './src/workspaceLayout';
import './src/app.css';
let callback=0;
window.__TAURI_INTERNALS__={transformCallback:()=>++callback};
const created=[],closed=[],writes=[],resized=[],acknowledged=[];
const channels=new Map(),pending=new Map();
let deferred=false,fail=false,stops=0,gatedResize=false;
const resizeGates=new Map();
terminalApi.create=async (id,threadId,cwd,cols,rows,channel)=>{
  created.push({id,threadId,cwd,cols,rows});channels.set(id,channel);
  if(fail){fail=false;throw Error('test: shell not found');}
  if(deferred){deferred=false;await new Promise(resolve=>pending.set(id,resolve));}
};
terminalApi.write=async(id,data)=>{writes.push({id,data});};
terminalApi.resize=async(id,cols,rows)=>{
  resized.push({id,cols,rows});
  if(gatedResize){gatedResize=false;await new Promise(resolve=>resizeGates.set(id,resolve));}
};
terminalApi.ack=async(id,bytes)=>{acknowledged.push({id,bytes});};
terminalApi.close=async id=>{closed.push(id);};
api.listWorkspaceDirectory=async()=>({entries:[],truncated:false});
setState({currentId:'a',cwd:'/project/a',theme:'ink-light',items:[]});
setWorkspaceLayout({open:true,mode:'terminal'});
const [thread,setThread]=createSignal('a');
const group=()=>getTerminalGroup('thread:'+thread());
const active=()=>group().tabs().find(tab=>tab.id===group().activeId());
window.termTest={created,closed,writes,resized,acknowledged,
 id:()=>active()?.id,status:()=>active()?.status(),tab:id=>group().tabs().find(tab=>tab.id===id),
 setTheme:theme=>{setState('theme',theme);document.documentElement.dataset.theme=theme;},
 buffer:()=>{const b=active().terminal.buffer.active;return Array.from({length:b.length},(_,i)=>b.getLine(i)?.translateToString(true)??'').join('\\n');},
 send:(id,event)=>channels.get(id).onmessage(event),
 switch:id=>{setState({currentId:id,cwd:'/project/'+id});setThread(id);},
 defer:()=>{deferred=true;},fail:()=>{fail=true;},resolve:id=>{pending.get(id)();pending.delete(id);},
 paste:text=>active().terminal.paste(text),stopCount:()=>stops,
 gateResize:()=>{gatedResize=true;},hasResizeGate:id=>resizeGates.has(id),
 releaseResize:id=>{resizeGates.get(id)?.();resizeGates.delete(id);},
 closeAll:async()=>{for(const id of ['a','b']){const g=getTerminalGroup('thread:'+id);for(const tab of g.tabs())await closeTerminalTab(g,tab);}},
};
function Fixture(){
 mountSessionShortcuts({allowedActions:['toggleTerminal','stopSession'],onToggleTerminal:()=>setWorkspaceLayout({open:!workspaceLayout.open,mode:'terminal'}),onStopSession:()=>{stops++;return true;}});
 return <div style="display:flex;height:100vh;width:100vw"><main style="flex:1;min-width:0">Terminal lifecycle test</main><Show when={workspaceLayout.open}><Show when={thread()} keyed>{id=><WorkspacePanel threadId={id} request={null} onClose={()=>setWorkspaceLayout({open:false})}/>}</Show></Show></div>;
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
  await page.goto(`http://127.0.0.1:${port}/${name}.html`,{waitUntil:'domcontentloaded'});
  await page.waitForFunction(()=>window.termTest?.status()==='running');
  const first=await page.evaluate(()=>window.termTest.id());
  assert.equal(await page.evaluate(()=>window.termTest.created.length),1);
  assert.equal(await page.evaluate(()=>window.termTest.created[0].cwd),'/project/a');
  assert.equal(await page.locator('.workspace-file-view').isVisible(),false);
  assert.ok((await page.getByRole('tabpanel').boundingBox()).height>500);
  for(const theme of ['ink-light','ink-dark','ink-light']) {
    await page.evaluate(theme=>window.termTest.setTheme(theme),theme);
    await assertBlackTerminal();
  }
  assert.deepEqual(await page.locator('.workspace-terminal [role=tab]').allTextContents(),['终端 1']);
  assert.equal(await page.locator('.workspace-terminal-state').textContent(),'运行中');
  await assertStableTabsDuringOutput(first);
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
  assert.equal(await page.evaluate(()=>window.termTest.stopCount()),0);
  await page.keyboard.press('Control+Backquote');
  await page.waitForFunction(()=>!document.querySelector('.workspace-terminal'));
  assert.deepEqual(await page.evaluate(()=>window.termTest.closed),[]);
  await page.keyboard.press('Control+Shift+Backquote');
  await page.waitForFunction(()=>!!document.querySelector('.xterm-helper-textarea'));
  assert.equal(await page.evaluate(()=>window.termTest.created.length),1);
  assert.match(await page.evaluate(()=>window.termTest.buffer()),/中文🙂 output/);
  await assertBlackTerminal();
  assert.deepEqual(await page.locator('.workspace-terminal [role=tab]').allTextContents(),['终端 1']);
  await page.evaluate(()=>window.termTest.setTheme('ink-dark'));
  await page.getByRole('button',{name:'新建终端',exact:true}).click();
  await page.waitForFunction(()=>window.termTest.created.length===2&&window.termTest.status()==='running');
  const second=await page.evaluate(()=>window.termTest.id());
  await assertBlackTerminal();
  assert.deepEqual(await page.locator('.workspace-terminal [role=tab]').allTextContents(),['终端 1','终端 2']);
  await assertStableTabsDuringOutput(first); // An inactive tab must stay quiet too.
  await page.getByRole('tab',{name:'终端 1',exact:true}).click();
  assert.equal(await page.evaluate(()=>window.termTest.id()),first);
  await page.evaluate(()=>window.termTest.switch('b'));
  await page.waitForFunction(()=>window.termTest.created.length===3);
  await page.evaluate(()=>window.termTest.switch('a'));
  await page.waitForFunction(id=>window.termTest.id()===id,first);
  assert.equal(await page.getByRole('tab',{name:/终端/}).count(),2);
  await page.evaluate(()=>window.termTest.setTheme('ink-light'));
  await assertBlackTerminal();
  assert.deepEqual(await page.evaluate(()=>window.termTest.closed),[]);
  await page.evaluate(()=>window.termTest.paste('x'.repeat(70000)));
  await page.waitForFunction(()=>window.termTest.writes.reduce((n,w)=>n+w.data.filter(b=>b===120).length,0)===70000);
  assert.ok(await page.evaluate(()=>window.termTest.writes.every(w=>w.data.length<=32768)));
  await page.getByRole('button',{name:'关闭终端 终端 2',exact:true}).click();
  await page.waitForFunction(id=>window.termTest.closed.includes(id),second);
  await page.evaluate(()=>window.termTest.defer());
  await page.getByRole('button',{name:'新建终端',exact:true}).click();
  await page.waitForFunction(()=>window.termTest.created.length===4);
  const pending=await page.evaluate(()=>window.termTest.id());
  assert.equal(await page.locator('.workspace-terminal-state').textContent(),'启动中');
  assert.deepEqual(await page.locator('.workspace-terminal [role=tab]').allTextContents(),['终端 1','终端 2']);
  await page.getByRole('button',{name:'关闭终端 终端 2',exact:true}).click();
  assert.equal(await page.evaluate(id=>window.termTest.closed.includes(id),pending),false);
  await page.evaluate(id=>window.termTest.resolve(id),pending);
  await page.waitForFunction(id=>window.termTest.closed.includes(id),pending);
  await page.evaluate(()=>window.termTest.defer());
  await page.getByRole('button',{name:'新建终端',exact:true}).click();
  await page.waitForFunction(()=>window.termTest.created.length===5);
  await page.evaluate(()=>{const t=window.termTest;t.send(t.id(),{type:'exit',code:0});t.resolve(t.id());});
  await page.waitForFunction(()=>window.termTest.status()==='exited');
  assert.equal(await page.locator('.workspace-terminal-state').textContent(),'已退出');
  assert.deepEqual(await page.locator('.workspace-terminal [role=tab]').allTextContents(),['终端 1','终端 2']);
  await page.getByRole('button',{name:'关闭终端 终端 2',exact:true}).click();
  await page.evaluate(()=>window.termTest.fail());
  await page.getByRole('button',{name:'新建终端',exact:true}).click();
  await page.getByRole('alert').getByText('test: shell not found',{exact:false}).waitFor();
  assert.equal(await page.evaluate(()=>window.termTest.status()),'error');
  assert.equal(await page.locator('.workspace-terminal-state').textContent(),'启动失败');
  await page.getByRole('button',{name:'关闭终端 终端 2',exact:true}).click();
  const beforeResize=await page.evaluate(()=>({count:window.termTest.resized.length,id:window.termTest.id(),last:window.termTest.resized.filter(s=>s.id===window.termTest.id()).at(-1)}));
  await page.setViewportSize({width:1000,height:640});
  await page.waitForFunction(before=>window.termTest.resized.slice(before.count).some(s=>s.id===before.id&&(s.cols!==before.last.cols||s.rows!==before.last.rows)),beforeResize);
  assert.ok(await page.evaluate(()=>window.termTest.resized.every(s=>s.cols>0&&s.rows>0)));
  // Windows can wait for a cursor response inside the first resize. Input
  // and close must remain usable while that native resize is unresolved.
  await page.evaluate(()=>window.termTest.gateResize());
  await page.getByRole('button',{name:'新建终端',exact:true}).click();
  await page.waitForFunction(()=>window.termTest.hasResizeGate(window.termTest.id()));
  const gated=await page.evaluate(()=>window.termTest.id());
  const beforeGatedQuery=await page.evaluate(()=>window.termTest.writes.length);
  await page.evaluate(id=>window.termTest.send(id,{type:'data',data:[27,91,54,110]}),gated);
  await page.waitForFunction(before=>window.termTest.writes.slice(before).some(w=>w.data.at(-1)===82),beforeGatedQuery);
  await page.getByRole('button',{name:'关闭终端 终端 2',exact:true}).click();
  await page.waitForFunction(id=>window.termTest.closed.includes(id),gated);
  await page.evaluate(id=>window.termTest.releaseResize(id),gated);
  if(process.env.TEST_SCREENSHOT)await page.screenshot({path:process.env.TEST_SCREENSHOT});
  await page.evaluate(()=>window.termTest.closeAll());
  await page.waitForFunction(()=>window.termTest.closed.length===window.termTest.created.length);
  assert.deepEqual(errors,[]);
  console.log('Workspace terminal: fixed black theme, stable OSC titles/tab DOM, footer status, UTF-8, control keys, tabs, hide/remount, conversation retention, paste, spawn/close races, errors and resize passed');
} catch(error) { if(process.env.TEST_SCREENSHOT&&page)await page.screenshot({path:process.env.TEST_SCREENSHOT}).catch(()=>{}); throw error; } finally { await browser?.close();server?.kill();await Promise.all([`${name}.html`,`${name}.tsx`].map(path=>rm(path,{force:true}))); }
