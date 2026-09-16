// Windows + installed WebView2 runtime. No browser automation dependency.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { mkdtemp, mkdir, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { resolve, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { performance } from 'node:perf_hooks';

const root = fileURLToPath(new URL('../../', import.meta.url));
const output = resolve(process.argv[2] || join(root, 'src-tauri/target/webview-probe-results'));
await mkdir(output, { recursive: true });
const profile = await mkdtemp(join(tmpdir(), 'nova-webview-probe-'));
const frame = '<!doctype html><meta charset="utf-8"><button onclick="this.textContent=\'框架成功\'">框架按钮</button>';
const server = createServer((req, res) => {
  res.setHeader('Content-Type', 'text/html; charset=utf-8');
  res.end(req.url === '/frame' ? frame : `<!doctype html><meta charset="utf-8"><title>Native WebView fixture</title>
    <style>body{font:16px system-ui;padding:20px}section{padding:14px;border:1px solid #bbb;margin:12px 0}button,input{font:inherit;padding:8px}iframe{width:95%;height:70px}</style>
    <h1>订单工作台</h1><section aria-label="客户筛选"><h2>客户筛选</h2><button onclick="window.wrong=true">查询</button></section>
    <section aria-label="订单筛选"><h2>订单筛选</h2><label>订单号 <input aria-label="订单号"></label><button onclick="window.trusted=event.isTrusted;setTimeout(()=>document.querySelector('output').textContent=document.querySelector('input').value+' 已发货',120)">查询</button><output></output></section>
    <div id="shadow"></div><script>const host=document.querySelector('#shadow').attachShadow({mode:'open'});const button=document.createElement('button');button.textContent='Shadow 按钮';button.onclick=()=>button.textContent='Shadow 成功';host.append(button);</script>
    <iframe title="跨域框架" src="http://localhost:${server.address().port}/frame"></iframe>
    <div style="height:1100px">下面还有内容</div><button id="bottom" onclick="this.textContent='底部成功'">屏幕外按钮</button>`);
});
await new Promise(r => server.listen(0, '127.0.0.1', r));
const child = spawn(join(root, 'bench/webview-probe/target/debug/nova-webview-probe.exe'), [], {
  windowsHide: true, cwd: root,
  env: { ...process.env, NOVA_PROBE_URL: `http://127.0.0.1:${server.address().port}`, NOVA_PROBE_PROFILE: profile },
  stdio: ['pipe', 'pipe', 'pipe'],
});
let sequence = 0;
const pending = new Map();
const checks = [];
let stderr = '';
child.stderr.on('data', data => { stderr += data; });
const ready = new Promise((resolveReady, reject) => {
  const timer = setTimeout(() => reject(new Error('WebView startup timeout')), 30000);
  child.once('error', reject);
  child.once('exit', code => {
    clearTimeout(timer);
    reject(new Error(`Probe exited ${code}: ${stderr}`));
    for (const job of pending.values()) job.reject(new Error(`Probe exited ${code}`));
  });
  createInterface({ input: child.stdout }).on('line', line => {
    let message; try { message = JSON.parse(line); } catch { return; }
    if (message.ready) { clearTimeout(timer); resolveReady(message); }
    const job = pending.get(message.id);
    if (!job) return;
    pending.delete(message.id);
    clearTimeout(job.timer);
    if (message.error || message.result?.error) job.reject(new Error(JSON.stringify(message.error || message.result.error)));
    else job.resolve(message.result);
  });
});
function call(method, params = {}, sessionId) {
  return new Promise((resolve, reject) => {
    const id = ++sequence;
    const timer = setTimeout(() => { pending.delete(id); reject(new Error(`${method} timed out`)); }, 10000);
    pending.set(id, { resolve, reject, timer });
    child.stdin.write(JSON.stringify({ id, method, params, sessionId }) + '\n');
  });
}
async function evaluate(expression, contextId, sessionId) {
  const result = await call('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true, ...(contextId ? { contextId } : {}) }, sessionId);
  if (result.exceptionDetails) throw new Error(JSON.stringify(result.exceptionDetails));
  return result.result.value;
}
async function until(expression, contextId, sessionId) {
  const start = performance.now();
  while (performance.now() - start < 5000) {
    if (await evaluate(expression, contextId, sessionId)) return;
    await new Promise(r => setTimeout(r, 40));
  }
  throw new Error(`Condition timed out: ${expression}`);
}
async function click(expression) {
  const point = await evaluate(`(async()=>{const e=${expression};if(!e)throw Error('missing target');e.scrollIntoView({block:'center'});await new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)));const r=e.getBoundingClientRect();if(!r.width||!r.height)throw Error('invisible target');return {x:r.x+r.width/2,y:r.y+r.height/2}})()`);
  for (const type of ['mousePressed', 'mouseReleased']) {
    await call('Input.dispatchMouseEvent', { type, ...point, button: 'left', clickCount: 1 });
  }
}
async function check(name, action) {
  const start = performance.now();
  try { const detail = await action(); checks.push({ name, passed: true, ms: Math.round(performance.now() - start), detail }); }
  catch (error) { checks.push({ name, passed: false, ms: Math.round(performance.now() - start), error: String(error) }); }
  console.log(JSON.stringify(checks.at(-1)));
}
try {
  await ready;
  await until("document.querySelector('input') !== null");
  await check('right-side child WebView bounds', async () => {
    const bounds = await call('probe.bounds');
    assert.equal(bounds.x / bounds.scale, 400);
    assert.equal(bounds.width / bounds.scale, 800);
    return bounds;
  });
  await check('native accessibility tree observes duplicate buttons and Chinese labels', async () => {
    const tree = await call('Accessibility.getFullAXTree');
    assert.equal(tree.nodes.filter(n => n.role?.value === 'button' && n.name?.value === '查询').length, 2);
    assert.ok(tree.nodes.some(n => n.role?.value === 'textbox' && n.name?.value === '订单号'));
    await writeFile(join(output, 'accessibility.json'), JSON.stringify(tree, null, 2));
    return { nodes: tree.nodes.length };
  });
  await check('native Chinese input + scoped duplicate-button click + delayed result', async () => {
    await click("document.querySelector('input')");
    await call('Input.insertText', { text: '订单-测试-123' });
    await click("document.querySelector('section[aria-label=\"订单筛选\"] button')");
    await until("document.querySelector('output').textContent === '订单-测试-123 已发货'");
    assert.equal(await evaluate('window.trusted'), true);
    assert.notEqual(await evaluate('window.wrong'), true);
  });
  await check('open Shadow DOM target', async () => {
    await click("document.querySelector('#shadow').shadowRoot.querySelector('button')");
    assert.equal(await evaluate("document.querySelector('#shadow').shadowRoot.querySelector('button').textContent"), 'Shadow 成功');
  });
  await check('cross-origin iframe read and native click', async () => {
    const targets = await call('Target.getTargets');
    const target = targets.targetInfos.find(t => t.type === 'iframe' && t.url.includes('/frame'));
    assert.ok(target, 'cross-origin frame must be discoverable');
    const { sessionId } = await call('Target.attachToTarget', { targetId: target.targetId, flatten: true });
    await until("document.querySelector('button') !== null", undefined, sessionId);
    await evaluate("document.querySelector('iframe').scrollIntoView({block:'center'});new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)))");
    const outer = await evaluate("(()=>{const e=document.querySelector('iframe'),r=e.getBoundingClientRect();return {x:r.x+e.clientLeft,y:r.y+e.clientTop}})()");
    const inner = await evaluate("(()=>{const r=document.querySelector('button').getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2}})()", undefined, sessionId);
    await writeFile(join(output, 'frame-debug.json'), JSON.stringify({outer, inner, frame: await evaluate('document.documentElement.outerHTML', undefined, sessionId), top: await evaluate('document.documentElement.outerHTML')}, null, 2));
    for (const type of ['mousePressed', 'mouseReleased']) await call('Input.dispatchMouseEvent', { type, x: outer.x + inner.x, y: outer.y + inner.y, button: 'left', clickCount: 1 });
    await until("document.querySelector('button').textContent === '框架成功'", undefined, sessionId);
  });
  await check('offscreen target scroll and native click', async () => {
    await click("document.querySelector('#bottom')");
    await until("document.querySelector('#bottom').textContent === '底部成功'");
  });
  await check('native screenshot', async () => {
    await evaluate('window.scrollTo(0,0)');
    const shot = await call('Page.captureScreenshot', { format: 'png' });
    const data = Buffer.from(shot.data, 'base64');
    assert.equal(data.subarray(1, 4).toString(), 'PNG');
    await writeFile(join(output, 'page.png'), data);
    return { bytes: data.length };
  });
  await check('compact observation latency and payload (no model)', async () => {
    const expression = `Array.from(document.querySelectorAll('button,input')).map((e,i)=>({ref:i,role:e.tagName,name:e.getAttribute('aria-label')||e.textContent,region:e.closest('section')?.getAttribute('aria-label'),visible:e.getBoundingClientRect().bottom>0&&e.getBoundingClientRect().top<innerHeight}))`;
    const times = [];
    let snapshot;
    for (let i = 0; i < 20; i++) { const start = performance.now(); snapshot = await evaluate(expression); times.push(performance.now() - start); }
    times.sort((a,b) => a-b);
    await writeFile(join(output, 'observation.json'), JSON.stringify(snapshot, null, 2));
    return { samples: times.length, p50Ms: +times[9].toFixed(2), p95Ms: +times[18].toFixed(2), snapshotBytes: Buffer.byteLength(JSON.stringify(snapshot)), htmlBytes: await evaluate('new TextEncoder().encode(document.documentElement.outerHTML).length') };
  });
} finally {
  child.stdin.end(JSON.stringify({ method: 'probe.close' }) + '\n');
  const killTimer = setTimeout(() => child.kill(), 5000);
  await new Promise(r => { if (child.exitCode !== null) r(); else child.once('exit', r); });
  clearTimeout(killTimer);
  server.closeAllConnections();
  await new Promise(r => server.close(r));
  await writeFile(join(output, 'report.json'), JSON.stringify({ backend: 'Tauri child WebView2 + native COM CDP', playwright: false, modelTested: false, profile, checks, stderr }, null, 2));
}
assert.equal(checks.length, 8);
assert.equal(checks.filter(c => !c.passed).length, 0, `See ${join(output, 'report.json')}`);
console.log(`PASS: ${checks.length} checks; results: ${output}`);
