// Browser regression using production Solid/xterm components and IPC mocks only.
import assert from 'node:assert/strict';
import { access, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { spawn } from 'node:child_process';
import path from 'node:path';
import { chromium } from 'playwright-core';

const fixture = `terminal-home-theme-${process.pid}`;
const output = process.env.TEST_OUTPUT_DIR || 'terminal-home-theme-results';
let server, browser, page;
const report = [];
const record = name => { report.push({ name, passed: true }); console.log(`PASS ${name}`); };
try {
  await mkdir(output, { recursive: true });
  // Guard the real App wiring as well as exercising its shared state controller.
  const app = await readFile('src/App.tsx', 'utf8');
  assert.match(app, /const homeTerminal = createHomeTerminalState/);
  assert.match(app, /if \(!state\.currentId\) \{ homeTerminal\.toggle\(\); return; \}/);
  assert.match(app, /<Show when=\{homeTerminal\.open\(\)\}>/);
  assert.match(app, /<HomeTerminalPanel onClose=\{homeTerminal\.close\}/);
  assert.doesNotMatch(app, /!state\.currentId && workspaceLayout\.open/);
  await writeFile(`${fixture}.html`, '<div id="root"></div><script type="module" src="/' + fixture + '.tsx"></script>');
  await writeFile(`${fixture}.tsx`, `
import { render } from 'solid-js/web';
import { Show } from 'solid-js';
import HomeTerminalPanel from './src/components/HomeTerminalPanel';
import WorkspacePanel from './src/components/WorkspacePanel';
import { createHomeTerminalState, workspaceLayout, setWorkspaceLayout, setHomeTerminalCwd } from './src/workspaceLayout';
import { terminalApi, getTerminalGroup } from './src/terminalSessions';
import { state, setState, setTheme, openNewSession } from './src/store';
import { api } from './src/ipc';
import { mountSessionShortcuts } from './src/sessionShortcuts';
import './src/app.css';
let sequence=0;
window.__TAURI_INTERNALS__={transformCallback:()=>++sequence};
const created=[],closed=[],writes=[];
terminalApi.create=async(id,threadId,cwd,cols,rows,channel)=>{created.push({id,threadId,cwd});};
terminalApi.close=async id=>{closed.push(id);};
terminalApi.write=async(id,data)=>{writes.push({id,data});};
terminalApi.resize=async()=>{};terminalApi.ack=async()=>{};
api.listWorkspaceDirectory=async()=>({entries:[],truncated:false});
api.reportActivity=async()=>{};
setState({currentId:null,view:'home',homeComposerFocusAt:0,items:[],cwd:'/project/chat'});
setTheme('ink-light');setHomeTerminalCwd('/project/home');
function Fixture(){
  const home=createHomeTerminalState(()=>({currentId:state.currentId,view:state.view,homeComposerFocusAt:state.homeComposerFocusAt}));
  const group=()=>getTerminalGroup(state.currentId?'thread:'+state.currentId:'home');
  const active=()=>group().tabs().find(t=>t.id===group().activeId());
  mountSessionShortcuts({allowedActions:['toggleTerminal'],onToggleTerminal:()=>{
    if(!state.currentId){home.toggle();return;}
    setWorkspaceLayout({open:!(workspaceLayout.open&&workspaceLayout.mode==='terminal'),mode:'terminal'});
  }});
  window.homeTest={created,closed,writes,home,group,active,layout:()=>({...workspaceLayout}),
    theme:setTheme,newSession:openNewSession,
    route:(id,view='home')=>setState({currentId:id,view}),
    write:text=>new Promise(resolve=>active().terminal.write(text,resolve)),
  };
  return <div style="display:flex;width:100vw;height:100vh">
    <main style="flex:1;min-width:0">Home terminal regression</main>
    <Show when={state.currentId&&workspaceLayout.open}><WorkspacePanel threadId={state.currentId!} request={null} onClose={()=>setWorkspaceLayout({open:false})}/></Show>
    <Show when={home.open()}><HomeTerminalPanel onClose={home.close}/></Show>
  </div>;
}
render(()=><Fixture/>,document.getElementById('root')!);
`);
  const port = 16000 + process.pid % 1000;
  server = spawn(process.execPath, ['node_modules/vite/bin/vite.js', '--host', '127.0.0.1', '--port', String(port), '--strictPort'],
    { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'], env: { ...process.env, NO_COLOR: '1' } });
  await new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error('Vite startup timed out')), 30000);
    server.stdout.on('data', data => { if (data.toString().includes('Local:')) { clearTimeout(timeout); resolve(); } });
    server.stderr.on('data', data => process.stderr.write(data));
    server.once('error', error => { clearTimeout(timeout); reject(error); });
    server.once('exit', code => { clearTimeout(timeout); reject(new Error(`Vite exited: ${code}`)); });
  });
  let executablePath;
  for (const candidate of [process.env.TEST_BROWSER, '/usr/bin/google-chrome', '/usr/bin/chromium', 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe'].filter(Boolean)) {
    try { await access(candidate); executablePath = candidate; break; } catch { /* Try next browser. */ }
  }
  assert.ok(executablePath, 'No test browser found; set TEST_BROWSER.');
  browser = await chromium.launch({ executablePath, headless: true, args: ['--no-sandbox'] });
  page = await browser.newPage({ viewport: { width: 1280, height: 820 } });
  page.setDefaultTimeout(15000);
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.addInitScript(() => localStorage.setItem('fd:workspaceLayout', JSON.stringify({open:true,mode:'terminal',widthRatio:.38,minimap:true,softWrap:true})));
  await page.goto(`http://127.0.0.1:${port}/${fixture}.html`, { waitUntil: 'networkidle' });
  await page.waitForFunction(() => !!window.homeTest);
  assert.equal(await page.locator('.workspace-terminal').count(), 0);
  assert.equal(await page.evaluate(() => window.homeTest.created.length), 0);
  const remembered = await page.evaluate(() => localStorage.getItem('fd:workspaceLayout'));
  record('saved-open terminal does not mount or spawn on a new-session page');

  await page.keyboard.press('Control+Backquote');
  await page.waitForFunction(() => window.homeTest.active()?.status() === 'running');
  const first = await page.evaluate(() => window.homeTest.active().id);
  assert.equal(await page.evaluate(() => window.homeTest.created.length), 1);
  assert.equal(await page.evaluate(() => window.homeTest.active().cwd), '/project/home');
  record('first explicit shortcut opens home terminal even when saved chat state is open');

  // Spaces with opaque cell backgrounds cover ANSI, indexed and truecolor paths.
  await page.evaluate(() => window.homeTest.write('\x1b[2J\x1b[H\x1b[41m  \x1b[0m\x1b[48;5;202m  \x1b[0m\x1b[48;2;12;150;225m  \x1b[0m\r\nANSI / indexed / truecolor\r\n'));
  const sample = async theme => {
    await page.evaluate(theme => window.homeTest.theme(theme), theme);
    const info = await page.evaluate(() => {
      const surface = document.querySelector('.workspace-terminal-surface');
      const screen = surface.querySelector('.xterm-screen').getBoundingClientRect();
      const rect = surface.getBoundingClientRect(), tab = window.homeTest.active();
      const cellWidth = screen.width / tab.terminal.cols, cellHeight = screen.height / tab.terminal.rows;
      return { id: tab.id, theme: tab.terminal.options.theme, filter: getComputedStyle(surface).filter,
        toolbarFilter: getComputedStyle(document.querySelector('.workspace-terminal .workspace-toolbar')).filter,
        footerFilter: getComputedStyle(document.querySelector('.workspace-terminal-status')).filter,
        points: [[rect.left + 1, rect.top + 1], ...[.75,2.75,4.75].map(col => [screen.left + col * cellWidth, screen.top + .5 * cellHeight])] };
    });
    const png = await page.screenshot({ path: path.join(output, `terminal-${theme}.png`) });
    const pixels = await page.evaluate(async ({base64,points}) => {
      const img = new Image(); img.src = 'data:image/png;base64,' + base64; await img.decode();
      const canvas = document.createElement('canvas'); canvas.width=img.width;canvas.height=img.height;
      const ctx=canvas.getContext('2d');ctx.drawImage(img,0,0);
      return points.map(([x,y])=>Array.from(ctx.getImageData(Math.floor(x),Math.floor(y),1,1).data).slice(0,3));
    }, { base64: png.toString('base64'), points: info.points });
    assert.equal(info.id, first); assert.equal(info.theme.background, '#000000');
    assert.equal(info.toolbarFilter, 'none'); assert.equal(info.footerFilter, 'none');
    return { ...info, pixels };
  };
  const dark = await sample('ink-dark');
  const light = await sample('ink-light');
  assert.equal(dark.filter, 'none');
  assert.equal(light.filter, 'invert(1) hue-rotate(180deg)');
  assert.deepEqual(dark.pixels[0], [0,0,0]);
  assert.deepEqual(light.pixels[0], [255,255,255]);
  assert.deepEqual(dark.pixels[3], [12,150,225]);
  const inverted = rgb => {
    const [r,g,b] = rgb.map(v => 255-v);
    // W3C hue-rotate(180deg) color matrix, after per-channel invert().
    return [-.574*r + 1.43*g + .144*b, .426*r + .43*g + .144*b, .426*r + 1.43*g - .856*b].map(v => Math.round(Math.min(255,Math.max(0,v))));
  };
  for (let i=1;i<4;i++) {
    const expected=inverted(dark.pixels[i]);
    assert.ok(light.pixels[i].every((v,j)=>Math.abs(v-expected[j])<=4), `palette ${i}: ${light.pixels[i]} != ${expected}`);
  }
  const restored = await sample('ink-dark');
  assert.deepEqual(restored.pixels, dark.pixels);
  assert.equal(await page.evaluate(() => window.homeTest.created.length), 1);
  await writeFile(path.join(output,'palette-pixels.json'),JSON.stringify({dark,light,restored},null,2));
  record('actual screenshot pixels invert only the terminal; ANSI-16/256/truecolor and dark restoration verified');

  const stable = await page.evaluate(async () => {
    const bar=document.querySelector('[aria-label="终端标签页"]'),before=bar.innerHTML;
    let count=0;const observer=new MutationObserver(records=>count+=records.length);
    observer.observe(bar,{childList:true,characterData:true,attributes:true,subtree:true});
    for(let i=0;i<40;i++) await window.homeTest.write(`\x1b]2;progress ${i}\x07line ${i}\r\n`);
    count+=observer.takeRecords().length;observer.disconnect();
    return {count,same:bar.innerHTML===before,text:bar.querySelector('[role=tab]').textContent};
  });
  assert.deepEqual(stable,{count:0,same:true,text:'终端 1'});
  record('output and OSC title changes do not mutate the tab bar');

  await page.evaluate(() => window.homeTest.theme('ink-light'));
  await page.getByRole('button',{name:'新建终端',exact:true}).click();
  await page.waitForFunction(() => window.homeTest.group().tabs().length===2 && window.homeTest.active()?.status()==='running');
  assert.equal(await page.locator('.workspace-terminal-surface').evaluate(el=>getComputedStyle(el).filter),'invert(1) hue-rotate(180deg)');
  await page.getByRole('tab',{name:'终端 1',exact:true}).click();
  await page.getByRole('button',{name:'收起终端面板',exact:true}).click();
  await page.waitForFunction(() => !document.querySelector('.workspace-terminal'));
  assert.equal(await page.evaluate(() => localStorage.getItem('fd:workspaceLayout')),remembered);
  assert.deepEqual(await page.evaluate(() => window.homeTest.closed),[]);
  await page.keyboard.press('Control+Backquote');
  await page.waitForFunction(id=>window.homeTest.active()?.id===id&&!!document.querySelector('.workspace-terminal'),first);
  assert.equal(await page.evaluate(() => window.homeTest.created.length),2);
  record('light-mode new tabs and hide/reopen retain processes and do not overwrite chat preferences');

  await page.evaluate(() => window.homeTest.route('chat-a'));
  await page.waitForFunction(() => window.homeTest.created.length===3&&window.homeTest.active()?.status()==='running');
  assert.equal(await page.locator('.home-terminal-panel').count(),0);
  assert.equal(await page.evaluate(() => window.homeTest.layout().open),true);
  await page.evaluate(() => window.homeTest.route(null));
  await page.waitForFunction(() => !document.querySelector('.workspace-terminal'));
  assert.equal(await page.evaluate(() => window.homeTest.created.length),3);
  await page.keyboard.press('Control+Backquote');
  await page.waitForFunction(() => !!document.querySelector('.home-terminal-panel'));
  assert.equal(await page.evaluate(() => window.homeTest.active().id),first);
  record('entering home from a remembered-open chat stays closed; explicit reopen reuses home group');

  await page.evaluate(() => window.homeTest.newSession());
  await page.waitForFunction(() => !document.querySelector('.workspace-terminal'));
  assert.equal(await page.evaluate(() => window.homeTest.created.length),3);
  for(const view of ['workflows','clues']) {
    await page.evaluate(view=>window.homeTest.route(null,view),view);
    await page.keyboard.press('Control+Backquote');
    assert.equal(await page.locator('.workspace-terminal').count(),0);
  }
  await page.evaluate(() => window.homeTest.route(null));
  await page.keyboard.press('Control+Backquote');
  await page.waitForFunction(() => !!document.querySelector('.home-terminal-panel'));
  await page.keyboard.press('Control+Backquote');
  await page.waitForFunction(() => !document.querySelector('.workspace-terminal'));
  assert.equal(await page.evaluate(() => localStorage.getItem('fd:workspaceLayout')),remembered);
  record('new-session request resets visibility; non-home pages never open home terminal');

  await page.reload({waitUntil:'networkidle'});
  await page.waitForFunction(() => !!window.homeTest);
  assert.equal(await page.evaluate(() => window.homeTest.created.length),0);
  assert.equal(await page.locator('.workspace-terminal').count(),0);
  assert.deepEqual(errors,[]);
  record('application reload ignores the saved-open flag and never spawns a hidden terminal');
} catch(error) {
  report.push({passed:false,error:String(error.stack||error)});
  if(page)await page.screenshot({path:path.join(output,'failure.png')}).catch(()=>{});
  throw error;
} finally {
  await writeFile(path.join(output,'report.json'),JSON.stringify(report,null,2)).catch(()=>{});
  await browser?.close();server?.kill();
  await Promise.all([`${fixture}.html`,`${fixture}.tsx`].map(file=>rm(file,{force:true})));
}
