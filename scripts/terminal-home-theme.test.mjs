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
  // Guard the real App / WorkspacePanel / Sidebar wiring.
  const app = await readFile('src/App.tsx', 'utf8');
  assert.doesNotMatch(app, /HomeTerminalPanel|createHomeTerminalState|homeTerminal|toggleTerminal/);
  assert.match(app, /TerminalSessionView/);
  assert.match(app, /currentIsTerminal/);
  const panel = await readFile('src/components/WorkspacePanel.tsx', 'utf8');
  assert.doesNotMatch(panel, /WorkspaceTerminal|mode\(\) === "terminal"/);
  const sidebar = await readFile('src/components/Sidebar.tsx', 'utf8');
  assert.match(sidebar, /新终端/);
  assert.match(sidebar, /customTerminalMenus/);
  const settingsModal = await readFile('src/components/SettingsModal.tsx', 'utf8');
  assert.match(settingsModal, /自定义终端菜单/);

  await writeFile(`${fixture}.html`, '<div id="root"></div><script type="module" src="/' + fixture + '.tsx"></script>');
  await writeFile(`${fixture}.tsx`, `
import { render } from '@solidjs/web';
import { flush } from 'solid-js';
import { Show } from 'solid-js';
import TerminalSessionView from './src/components/TerminalSessionView';
import WorkspacePanel from './src/components/WorkspacePanel';
import { workspaceLayout, setWorkspaceLayout, syncWorkspaceOpen } from './src/workspaceLayout';
import { terminalApi, getTerminalGroup } from './src/terminalSessions';
import { state, setState, setTheme, openNewSession } from './src/store';
import { api } from './src/ipc';
import './src/app.css';
const created=[],closed=[],writes=[];
terminalApi.create=async(id,threadId,cwd,cols,rows,channel)=>{created.push({id,threadId,cwd});};
terminalApi.close=async id=>{closed.push(id);};
terminalApi.write=async(id,data)=>{writes.push({id,data});};
terminalApi.resize=async()=>{};terminalApi.ack=async()=>{};
api.listWorkspaceDirectory=async()=>({entries:[],truncated:false});
api.reportActivity=async()=>{};
flush(() => setState(s => {
  Object.assign(s, {
    currentId:null,
    view:'home',
    homeComposerFocusAt:0,
    items:[],
    cwd:'/project/term',
    title:'终端',
    threads:[
      { id:'term-1', title:'终端', cwd:'/project/term', agentKind:'lyra', createdAt:1, updatedAt:1, running:false, starred:false, unreadTurns:0, terminalThread:true },
      { id:'term-preset', title:'Plain Shell', cwd:'/project/term', agentKind:'lyra', createdAt:1, updatedAt:1, running:false, starred:false, unreadTurns:0, terminalThread:true, terminalConfig:{ initCommand:'', shell:'', args:[] } },
      { id:'chat-a', title:'对话 A', cwd:'/project/chat', agentKind:'lyra', createdAt:2, updatedAt:2, running:false, starred:false, unreadTurns:0 },
    ],
    settings:{ terminalInitCommand:'echo global-cmd', terminalFontSize:13 },
  });
}));
setTheme('ink-light');
function Fixture(){
  const currentIsTerminal = () => !!state.threads.find(t => t.id === state.currentId)?.terminalThread;
  const group=()=>state.currentId?getTerminalGroup('thread:'+state.currentId):undefined;
  const active=()=>group()?.tabs().find(t=>t.id===group()?.activeId());
  window.homeTest={created,closed,writes,group,active,layout:()=>({...workspaceLayout}),
    theme:setTheme,newSession:openNewSession,
    openPanel:()=>setWorkspaceLayout({open:true,mode:'files'}),
    route:(id,view='home')=>{
      const meta=state.threads.find(t=>t.id===id);
      setState(s=>{Object.assign(s,{currentId:id,view,cwd:meta?.cwd??'',title:meta?.title??''});});
      syncWorkspaceOpen(id);
    },
    write:text=>new Promise(resolve=>active().terminal.write(text,resolve)),
  };
  return <div style="display:flex;width:100vw;height:100vh">
    <Show when={state.currentId} fallback={<main style="flex:1;min-width:0">Home</main>}>
      <Show when={currentIsTerminal()} fallback={<main style="flex:1;min-width:0;display:flex"><div style="flex:1">Chat</div><Show when={workspaceLayout.open}><WorkspacePanel threadId={state.currentId!} request={null} onClose={()=>setWorkspaceLayout({open:false})}/></Show></main>}>
        <Show keyed when={state.currentId}>{id => <TerminalSessionView threadId={id}/>}</Show>
      </Show>
    </Show>
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
  await page.addInitScript(() => {
    let sequence = 0;
    window.__TAURI_INTERNALS__ = {
      transformCallback: () => ++sequence,
      metadata: { currentWindow: { label: 'main' }, currentWebview: { label: 'main' } },
    };
    localStorage.setItem('fd:workspaceLayout', JSON.stringify({open:true,mode:'terminal',widthRatio:.38,minimap:true,softWrap:true}));
  });
  await page.goto(`http://127.0.0.1:${port}/${fixture}.html`, { waitUntil: 'networkidle' });
  await page.waitForFunction(() => !!window.homeTest);
  assert.equal(await page.locator('.workspace-terminal').count(), 0);
  assert.equal(await page.evaluate(() => window.homeTest.created.length), 0);
  assert.equal(await page.evaluate(() => window.homeTest.layout().mode), 'files');
  record('legacy terminal mode falls back to files and home never mounts terminal');

  await page.evaluate(() => window.homeTest.route('term-1'));
  await page.waitForFunction(() => window.homeTest.active()?.status() === 'running');
  const first = await page.evaluate(() => window.homeTest.active().id);
  assert.equal(await page.evaluate(() => window.homeTest.created.length), 1);
  assert.equal(await page.evaluate(() => window.homeTest.active().cwd), '/project/term');
  record('terminal session mounts interactive terminal in main view');

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
    assert.equal(info.footerFilter, 'none');
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
    const bar=document.querySelector('.workspace-terminal-status'),before=bar.innerHTML;
    let count=0;const observer=new MutationObserver(records=>count+=records.length);
    observer.observe(bar,{childList:true,characterData:true,attributes:true,subtree:true});
    for(let i=0;i<40;i++) await window.homeTest.write(`\x1b]2;progress ${i}\x07line ${i}\r\n`);
    count+=observer.takeRecords().length;observer.disconnect();
    return {count,same:bar.innerHTML===before,tabs:document.querySelectorAll('.workspace-terminal [role=tab]').length};
  });
  assert.deepEqual(stable,{count:0,same:true,tabs:0});
  record('output and OSC title changes do not mutate the status bar');

  await page.evaluate(() => window.homeTest.route('chat-a'));
  await page.waitForFunction(() => !document.querySelector('.workspace-terminal'));
  await page.evaluate(() => window.homeTest.openPanel());
  await page.waitForSelector('.workspace-panel');
  assert.equal(await page.locator('.workspace-panel').count(), 1);
  assert.equal(await page.locator('.workspace-modes button', { hasText: '终端' }).count(), 0);
  assert.deepEqual(await page.evaluate(() => window.homeTest.closed), []);
  await page.evaluate(() => window.homeTest.route('term-preset'));
  await page.waitForFunction(() => window.homeTest.created.at(-1)?.threadId === 'term-preset' && window.homeTest.active()?.status() === 'running');
  assert.equal(await page.evaluate(() => window.homeTest.active().initCommand), '');
  await page.evaluate(() => window.homeTest.route('term-1'));
  await page.waitForFunction(id => window.homeTest.active()?.id === id && !!document.querySelector('.workspace-terminal'), first);
  assert.equal(await page.evaluate(() => window.homeTest.created.length), 2);
  assert.equal(await page.evaluate(() => window.homeTest.active().initCommand), 'echo global-cmd');
  assert.deepEqual(errors, []);
  record('chat sidebar has no terminal tab, custom preset overrides global command, and returning to terminal session retains PTY');
} catch(error) {
  report.push({passed:false,error:String(error.stack||error)});
  if(page)await page.screenshot({path:path.join(output,'failure.png')}).catch(()=>{});
  throw error;
} finally {
  await writeFile(path.join(output,'report.json'),JSON.stringify(report,null,2)).catch(()=>{});
  await browser?.close();server?.kill();
  await Promise.all([`${fixture}.html`,`${fixture}.tsx`].map(file=>rm(file,{force:true})));
}
