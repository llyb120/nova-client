// Live A/B: a Lyra session (default commandcode/claude-haiku-5-5) drives Chrome on a local English
// report page from a Chinese request; NOVA_JEV=off vs on. The page server records what was really
// applied, so success never relies on the model's claims.
// Run from repo root with `npm run tauri dev`'s target/debug/nova.exe built and the Nova Chrome
// extension loaded:  node bench/jev-run-live.mjs [--variants off,on] [--runs 1] [--model id]
// --self-test checks the fixture's scoring without Nova or Chrome.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { mkdtemp, mkdir, copyFile, readFile, writeFile } from 'node:fs/promises';
import { tmpdir, homedir } from 'node:os';
import { join, resolve } from 'node:path';
import { parseArgs } from 'node:util';

const { values: opt } = parseArgs({ options: { variants: { type: 'string', default: 'off,on' }, runs: { type: 'string', default: '1' },
  model: { type: 'string', default: 'commandcode/claude-haiku-5-5' }, 'self-test': { type: 'boolean' }, direct: { type: 'boolean' }, hint: { type: 'string', default: '' }, agent: { type: 'string', default: 'lyra' } } });
// --agent codebuddy --model deepseek-v4.1-flash:high drives CodeBuddy ACP (chrome via nova-tools MCP).
// --direct skips the model: one run with Chinese step descriptions of English controls (jev's job).
const sleep = ms => new Promise(r => setTimeout(r, ms));

const COUNTRIES = ['United States', 'United Kingdom', 'United Arab Emirates', 'Japan', 'Germany', 'France', 'Canada', 'Brazil', 'India', 'South Korea'];
const PERIODS = { 'Last 7 Days': 0.25, 'Last 30 Days': 1, 'Last 3 Months': 3, 'Last 6 Months': 6, 'Last 12 Months': 12 };
// Revenue factor per country (missing = 0.4); downloads factor. US top (Iron Tide) differs from global and downloads tops.
const GAMES = [
  ['Dune Raiders', { 'United States': 1.5, Brazil: 2.2 }, 3.0], ['Iron Tide', { 'United States': 4.2, Germany: 0.9 }, 1.1],
  ['Koi Garden', { 'United States': 0.9, Japan: 2.5 }, 9.5], ['Nebula Quest', { 'United States': 3.1, 'United Kingdom': 3.9 }, 2.4],
  ['Pixel Farm', { 'United States': 2.0, Japan: 9.0, 'South Korea': 4.0 }, 4.1], ['Star Rail Mini', { 'United States': 3.9, 'United Arab Emirates': 5.2 }, 1.8],
];
const table = (regions, months) => GAMES.map(([name, f, d]) => ({ name,
  revenue: Math.round((regions.length ? regions : COUNTRIES).reduce((s, c) => s + (f[c] ?? 0.4), 0) * months * 1000),
  downloads: Math.round(d * months * 100) }));
const expected = table(['United States'], 6).sort((a, b) => b.revenue - a.revenue)[0];

const PAGE = `<!doctype html><meta charset="utf-8"><title>Revenue Report</title>
<style>body{font:14px sans-serif;margin:24px}header{display:flex;gap:12px;margin-bottom:16px}.bar{display:flex;gap:16px;align-items:flex-start}
.f{position:relative}.panel{position:absolute;top:32px;left:0;background:#fff;border:1px solid #999;padding:8px;width:240px;z-index:2}
.panel label{display:block;padding:2px 0}th span.s{cursor:pointer;margin-left:4px;color:#666}table{margin-top:20px;border-collapse:collapse}td,th{border:1px solid #ccc;padding:4px 10px}</style>
<header><input placeholder="Search games" aria-label="Search games"><button onclick="alert('blocked')">Export</button><button>Logout</button></header>
<h1>Revenue Report</h1>
<div class="bar">
 <div class="f">Region <button id="rb" aria-haspopup="listbox">Global ▾</button><div id="rp" class="panel" hidden></div></div>
 <div class="f">Period <button id="pb" aria-haspopup="listbox">Last 30 Days ▾</button><ul id="pm" class="panel" role="listbox" hidden></ul></div>
 <button id="apply">Apply</button><button id="reset">Reset</button>
</div>
<p id="status">Showing: Global · Last 30 Days</p>
<table><thead><tr><th>Game</th><th>Revenue<span class="s" tabindex="0" data-k="revenue" data-d="asc">▲</span><span class="s" tabindex="0" data-k="revenue" data-d="desc">▼</span></th>
<th>Downloads<span class="s" tabindex="0" data-k="downloads" data-d="asc">▲</span><span class="s" tabindex="0" data-k="downloads" data-d="desc">▼</span></th></tr></thead><tbody id="rows"></tbody></table>
<script>
const COUNTRIES=${JSON.stringify(COUNTRIES)},PERIODS=${JSON.stringify(PERIODS)},GAMES=${JSON.stringify(GAMES)};
const table=${table.toString()};
let regions=[],draft=[],period='Last 30 Days',pendingPeriod=period,sort=null;
const $=s=>document.querySelector(s),post=(p,b)=>fetch(p,{method:'POST',body:JSON.stringify(b)});
function render(){let rows=table(regions,PERIODS[period]);if(sort)rows.sort((a,b)=>sort.d==='desc'?b[sort.k]-a[sort.k]:a[sort.k]-b[sort.k]);else rows.sort((a,b)=>a.name.localeCompare(b.name));
 $('#rows').innerHTML=rows.map(r=>'<tr><td>'+r.name+'</td><td>$'+r.revenue.toLocaleString('en-US')+'</td><td>'+r.downloads.toLocaleString('en-US')+'</td></tr>').join('');
 $('#status').textContent='Showing: '+(regions.join(', ')||'Global')+' · '+period+(sort?' · sorted by '+sort.k+' '+sort.d:'');}
$('#rb').onclick=()=>{draft=[...regions];setTimeout(()=>{const p=$('#rp');p.hidden=false;p.innerHTML='<input placeholder="Search" aria-label="Search">'+COUNTRIES.map(c=>'<label><input type="checkbox" value="'+c+'"'+(draft.includes(c)?' checked':'')+'> '+c+'</label>').join('')+'<button id="rc">Confirm</button> <button id="rx">Cancel</button>';
 p.querySelectorAll('input[type=checkbox]').forEach(b=>b.onchange=()=>{draft=b.checked?[...draft,b.value]:draft.filter(x=>x!==b.value)});
 p.querySelector('input').oninput=e=>p.querySelectorAll('label').forEach(l=>l.hidden=!l.textContent.toLowerCase().includes(e.target.value.toLowerCase()));
 $('#rc').onclick=()=>{regions=draft;$('#rb').textContent=(regions.join(', ')||'Global')+' ▾';p.hidden=true};$('#rx').onclick=()=>{p.hidden=true}},300)};
$('#pb').onclick=()=>{const m=$('#pm');m.hidden=false;m.innerHTML=Object.keys(PERIODS).map(k=>'<li role="option" style="cursor:pointer">'+k+'</li>').join('');
 m.querySelectorAll('li').forEach(li=>li.onclick=()=>{pendingPeriod=li.textContent;$('#pb').textContent=pendingPeriod+' ▾';m.hidden=true})};
$('#apply').onclick=()=>{period=pendingPeriod;$('#rows').innerHTML='<tr><td colspan=3>Loading…</td></tr>';post('/api/apply',{regions,period});setTimeout(render,600)};
$('#reset').onclick=()=>{regions=[];period=pendingPeriod='Last 30 Days';sort=null;render()};
document.querySelectorAll('span.s').forEach(s=>s.onclick=()=>{sort={k:s.dataset.k,d:s.dataset.d};post('/api/sort',sort);render()});
render();
</script>`;

function verdict(state, answer) {
  const applied = JSON.stringify(state.applied?.regions) === '["United States"]' && state.applied?.period === 'Last 6 Months';
  const sorted = state.sort?.k === 'revenue' && state.sort?.d === 'desc';
  const answered = answer.includes(expected.name) && answer.replace(/[,，\s]/g, '').includes(String(expected.revenue));
  return { applied, sorted, answered, success: applied && sorted && answered };
}

if (opt['self-test']) {
  assert.deepEqual(expected, { name: 'Iron Tide', revenue: 25200, downloads: 660 });
  const good = { applied: { regions: ['United States'], period: 'Last 6 Months' }, sort: { k: 'revenue', d: 'desc' } };
  assert.equal(verdict(good, '第一名是 Iron Tide，收入 $25,200').success, true);
  assert.equal(verdict({ ...good, sort: null }, 'Iron Tide 25,200').success, false);
  assert.equal(verdict(good, 'Star Rail Mini 23,400').success, false);
  console.log('self-test passed');
  process.exit(0);
}

async function runOnce(variant, index) {
  const state = { applied: null, sort: null, applies: 0 };
  const fixture = createServer((req, res) => {
    let body = '';
    req.on('data', c => body += c);
    req.on('end', () => {
      if (req.url === '/api/apply') { state.applied = JSON.parse(body); state.applies++; }
      else if (req.url === '/api/sort') state.sort = JSON.parse(body);
      res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
      res.end(req.url.startsWith('/api/') ? '{}' : PAGE);
    });
  });
  await new Promise(r => fixture.listen(0, '127.0.0.1', r));
  const url = `http://127.0.0.1:${fixture.address().port}/report`;
  const profile = await mkdtemp(join(tmpdir(), `nova-jev-live-${variant}-`));
  const source = join(homedir(), '.novadev');
  const settings = JSON.parse(await readFile(join(source, 'settings.json'), 'utf8'));
  const cb = opt.agent === 'codebuddy';
  await writeFile(join(profile, 'settings.json'), JSON.stringify({ relayServer: '', relayToken: '', sessionShortcuts: [], lyraEnabled: true, lyraProxy: settings.lyraProxy ?? '',
    codebuddyEnabled: cb, codebuddyPath: settings.codebuddyPath, codebuddyArgs: settings.codebuddyArgs, codebuddyProxy: settings.codebuddyProxy, codebuddyIntegration: settings.codebuddyIntegration,
    codexEnabled: false, claudeEnabled: false, kimiEnabled: false, devinEnabled: false, workbuddyEnabled: false }));
  await mkdir(join(profile, 'alkaid'));
  for (const f of ['config.jsonc', 'models-cache.json']) await copyFile(join(source, 'alkaid', f), join(profile, 'alkaid', f)).catch(() => {});
  const port = await new Promise(r => { const s = createServer(); s.listen(0, '127.0.0.1', () => { const p = s.address().port; s.close(() => r(p)); }); });
  const child = spawn(resolve('src-tauri/target/debug/nova.exe'), [], { windowsHide: true, stdio: 'ignore', env: { ...process.env, NOVA_JEV: variant, NOVA_DATA_DIR: profile,
    NOVA_CONTEXT_SERVICE_ENDPOINT: '', NOVA_CONTEXT_SERVICE_TOKEN: '',
    WEBVIEW2_USER_DATA_FOLDER: join(profile, 'webview-runtime'),
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port} --remote-debugging-address=127.0.0.1 --disable-features=CalculateNativeWinOcclusion` } });
  const report = { variant, index, model: opt.model, url, profile };
  let socket, invoke, thread;
  const started = Date.now();
  try {
    let target;
    for (let i = 0; i < 150 && !target; i++) { try { target = (await (await fetch(`http://127.0.0.1:${port}/json/list`)).json()).find(t => t.type === 'page' && /^http:\/\/127\.0\.0\.1:\d+\/$/.test(t.url)); } catch {} if (!target) await sleep(300); }
    assert(target, 'Nova debug UI target unavailable');
    socket = new WebSocket(target.webSocketDebuggerUrl);
    await new Promise((r, j) => { socket.onopen = r; socket.onerror = j; });
    let id = 0; const pending = new Map();
    socket.onmessage = ({ data }) => { const m = JSON.parse(data), p = pending.get(m.id); if (p) { pending.delete(m.id); m.error ? p.reject(Error(JSON.stringify(m.error))) : p.resolve(m.result); } };
    const cdp = (method, params) => new Promise((resolve, reject) => { const key = ++id; pending.set(key, { resolve, reject }); socket.send(JSON.stringify({ id: key, method, params })); setTimeout(() => pending.has(key) && (pending.delete(key), reject(Error(method + ' timeout'))), 90000); });
    const evaluate = async expression => { const r = await cdp('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true }); if (r.exceptionDetails) throw Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text); return r.result.value; };
    for (let i = 0; i < 150; i++) { if (await evaluate('!!window.__TAURI_INTERNALS__&&!!document.querySelector(".app")').catch(() => false)) break; await sleep(200); }
    invoke = async (command, args = {}) => {
      const r = await evaluate(`window.__TAURI_INTERNALS__.invoke(${JSON.stringify(command)},${JSON.stringify(args)}).then(v=>({v}),e=>({e:String(e)}))`);
      if ('e' in r) throw Error(`${command}: ${r.e}`);
      return r.v;
    };
    thread = await invoke('create_thread', { cwd: profile, agentKind: opt.agent, model: opt.model, mode: 'build', ephemeral: false });
    report.threadId = thread.id;
    // chrome_browser_ui acts for the visible thread.
    for (let i = 0; i < 50; i++) { if (await evaluate('!!document.querySelector(".thread-item")')) break; await sleep(200); }
    await evaluate('document.querySelector(".thread-item")?.click(); true');
    await invoke('report_activity', { threadId: thread.id }).catch(() => {});
    let status;
    for (let i = 0; i < 40; i++) { status = await invoke('chrome_browser_ui', { operation: 'connect', args: {} }).catch(e => ({ error: String(e) })); if (status.connected) break; await sleep(1000); }
    assert(status?.connected, 'Chrome extension did not connect: ' + JSON.stringify(status));
    if (opt.direct) {
      const chrome = (operation, args) => invoke('chrome_browser_ui', { operation, args });
      const page = await chrome('open', { url });
      const t = Date.now();
      const result = await chrome('run', { tabTag: page.tabTag, snapshotId: page.snapshotId, plan: { task: '地区只选美国、时间近半年并应用，再按收入从高到低排序',
        steps: [{ action: 'click', target: '地区筛选下拉按钮' }, { action: 'click', target: '美国 复选框' }, { action: 'click', target: '地区面板里的确认按钮' },
          { action: 'click', target: '时间筛选下拉按钮' }, { action: 'click', target: '近半年 选项' }, { action: 'click', target: '应用按钮' },
          { action: 'click', target: '收入列的降序图标', expect: '收入从高到低排序' }] } });
      report.wallMs = Date.now() - t;
      report.runs = [result.altairRun];
      report.answer = `${expected.name} ${expected.revenue}`;
      report.finished = true;
      return report;
    }
    const prompt = `打开 ${url} 这个收入报表（本地测试页，允许任何筛选和排序操作，不要导出或登出）。把地区只选美国、时间改成近半年并应用，再把表格按收入从高到低排序，最后告诉我排第一的游戏和它的收入。${opt.hint}`;
    await invoke('send_prompt', { threadId: thread.id, text: prompt });
    let latest;
    for (let i = 0; i < 160; i++) {
      await sleep(3000);
      latest = await invoke('get_thread', { threadId: thread.id });
      if ((latest.items ?? []).some(x => x.type === 'turn')) break;
    }
    report.wallMs = Date.now() - started;
    await writeFile(join(profile, 'thread.json'), JSON.stringify(latest, null, 2));
    const items = latest.items ?? [];
    report.finished = items.some(x => x.type === 'turn');
    report.answer = items.filter(i => i.type === 'assistant').at(-1)?.text ?? '';
    const tools = items.filter(i => i.type === 'tool');
    report.toolCalls = tools.length;
    report.chromeOps = tools.map(t => t.rawInput?.operation ?? t.title ?? '?');
    // Tool output is JSON, possibly wrapped in MCP text parts: unwrap strings until altairRun shows up.
    const runs = [];
    const walk = v => { if (typeof v === 'string') { if (v.includes('altairRun')) try { walk(JSON.parse(v)); } catch {} }
      else if (Array.isArray(v)) v.forEach(walk); else if (v && typeof v === 'object') { if (v.altairRun) runs.push(v.altairRun); else Object.values(v).forEach(walk); } };
    tools.forEach(t => walk(t.content ?? t.rawOutput));
    report.runs = runs.map(r => ({ status: r.status, reason: r.reason?.slice(0, 160), steps: r.guided?.totalSteps, done: r.guided?.completedSteps,
      local: r.guided?.resolvedLocally, byJev: r.guided?.resolvedByJev, checks: r.guided?.checks?.map(c => `${c.by}:${c.met}`), jev: r.jev, ms: r.elapsedMs, saved: r.experience?.saved ?? null }));
    const turn = items.findLast(i => i.type === 'turn');
    Object.assign(report, { inputTokens: turn?.inputTokens, outputTokens: turn?.outputTokens, assistantTurns: items.filter(i => i.type === 'assistant').length });
  } catch (error) { report.error = String(error); }
  finally {
    if (invoke && thread && !report.finished) await invoke('cancel_turn', { threadId: thread.id }).catch(() => {});
    if (invoke) try { for (const tab of (await invoke('chrome_browser_ui', { operation: 'tabs', args: {} })).tabs ?? []) if (tab.url?.startsWith(url)) await invoke('chrome_browser_ui', { operation: 'close_tab', args: { tabTag: tab.tag } }); } catch {}
    socket?.close(); child.kill(); fixture.close();
    Object.assign(report, { state, ...verdict(state, report.answer ?? '') });
  }
  return report;
}

const results = [];
for (let i = 0; i < Number(opt.runs); i++) for (const variant of opt.variants.split(',')) {
  const r = await runOnce(variant, i);
  results.push(r);
  console.log(JSON.stringify({ variant: r.variant, success: r.success, applied: r.applied, sorted: r.sorted, answered: r.answered, wallMs: r.wallMs,
    toolCalls: r.toolCalls, tokens: [r.inputTokens, r.outputTokens], ops: r.chromeOps?.join(','), runs: r.runs, error: r.error, answer: r.answer?.slice(0, 160), thread: join(r.profile, 'thread.json') }));
}
await writeFile(resolve('src-tauri/target/jev-run-live-report.json'), JSON.stringify(results, null, 2));
