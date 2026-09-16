// Standalone feasibility check. No Nova integration, Playwright, remote debugging port, or Chrome restart.
import { createServer } from 'node:http';
import { generateKeyPairSync, createHash, randomBytes, timingSafeEqual } from 'node:crypto';
import { readFile, writeFile, mkdir } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';
import assert from 'node:assert/strict';

const root = fileURLToPath(new URL('.', import.meta.url));
const output = join(root, 'out');
const extension = join(output, 'extension');
await mkdir(extension, { recursive: true });
let key;
try { key = await readFile(join(output, 'public-key.der')); }
catch (error) {
  if (error.code !== 'ENOENT') throw error;
  key = generateKeyPairSync('rsa', { modulusLength: 2048 }).publicKey.export({ type: 'spki', format: 'der' });
  await writeFile(join(output, 'public-key.der'), key);
}
const extensionId = [...createHash('sha256').update(key).digest('hex').slice(0, 32)].map(c => String.fromCharCode(97 + parseInt(c, 16))).join('');
let previous;
try { previous = JSON.parse(await readFile(join(extension, 'config.json'), 'utf8')); }
catch(error) { if (error.code !== 'ENOENT') throw error; }
// Restart the receiver at the address already installed in Chrome; never silently strand the extension.
if (previous) {
  const url = new URL(previous.origin);
  assert.equal(url.protocol, 'http:'); assert.equal(url.hostname, '127.0.0.1');
  assert.ok(Number(url.port) > 0 && !url.username && !url.password);
  assert.match(previous.token, /^[a-f0-9]{64}$/); assert.match(previous.nonce, /^[a-f0-9]{24}$/);
  assert.equal(previous.fixtureUrl, `${previous.origin}/fixture/${previous.nonce}`);
}
const token = previous?.token ?? randomBytes(32).toString('hex');
const nonce = previous?.nonce ?? randomBytes(12).toString('hex');
let fixtureUrl, reported = false;
const server = createServer(async (req, res) => {
  try {
    if (req.method === 'GET' && req.url === `/fixture/${nonce}`) {
      res.setHeader('Content-Type', 'text/html; charset=utf-8');
      res.setHeader('Cache-Control', 'no-store');
      res.end(`<!doctype html><meta charset="utf-8"><title>Nova Chrome 可行性测试</title><style>body{font:18px system-ui;padding:30px}input,button,a{padding:12px;margin:12px}#tail{margin-top:2200px}</style><h1>Nova Chrome 可行性测试</h1><p>仅测试此本地页面，不访问业务页面。请保持本标签打开，再加载验证扩展。</p><label>测试输入<input id="entry" oninput="window.inputTrusted=event.isTrusted"></label><button id="submit" onclick="window.clickTrusted=event.isTrusted;document.querySelector('output').textContent=document.querySelector('input').value">验证输入</button><output></output><a id="popup" href="/popup/${nonce}" target="_blank">新标签测试</a><div id="tail">整页底部标记 ${nonce}</div><script>localStorage.setItem('nova-probe-${nonce}','existing-session');window.beforeAttach='already-open';document.querySelector('input').value='existing-form';</script>`);
      return;
    }
    if (req.method === 'GET' && req.url === `/popup/${nonce}`) {
      res.setHeader('Content-Type', 'text/html; charset=utf-8');
      res.end('<!doctype html><title>Nova probe popup</title><p>仅由验证页面创建的新标签</p>');
      return;
    }
    const auth = Buffer.from(req.headers.authorization || '');
    const expected = Buffer.from(`Bearer ${token}`);
    if (req.headers.origin !== `chrome-extension://${extensionId}` || auth.length !== expected.length || !timingSafeEqual(auth, expected)) {
      res.writeHead(403); res.end('Forbidden'); return;
    }
    if (req.method !== 'POST' || req.url !== '/result') { res.writeHead(404); res.end(); return; }
    const chunks = []; let size = 0;
    for await (const chunk of req) { size += chunk.length; if (size > 12 * 1024 * 1024) throw Error('Result too large'); chunks.push(chunk); }
    const report = JSON.parse(Buffer.concat(chunks).toString());
    assert.equal(typeof report.passed, 'boolean');
    report.runId = nonce;
    report.receivedAt = new Date().toISOString();
    report.extensionId = extensionId;
    if (report.screenshot) {
      const png = Buffer.from(report.screenshot, 'base64');
      assert.equal(png.subarray(0, 8).toString('hex'), '89504e470d0a1a0a');
      report.image = { width: png.readUInt32BE(16), height: png.readUInt32BE(20), bytes: png.length };
      await writeFile(join(output, 'screenshot.png'), png);
      delete report.screenshot;
    }
    report.transport = 'Authenticated loopback HTTP from MV3 extension; no debugging port';
    await writeFile(join(output, 'report.json'), JSON.stringify(report, null, 2));
    reported = true;
    console.log(JSON.stringify(report, null, 2));
    res.end('saved');
  } catch (error) { console.error(error.message); res.writeHead(400); res.end('Invalid report'); }
});
await new Promise((resolve,reject) => {
  server.once('error', reject);
  server.listen(previous ? Number(new URL(previous.origin).port) : 0, '127.0.0.1', resolve);
});
const origin = `http://127.0.0.1:${server.address().port}`;
fixtureUrl = `${origin}/fixture/${nonce}`;
await Promise.all([
  writeFile(join(extension, 'manifest.json'), JSON.stringify({
    manifest_version: 3, name: 'Nova Chrome 验证助手', version: '0.0.3', key: key.toString('base64'),
    description: 'Temporary local-only debugger probe. Only attaches to the exact generated localhost test page.',
    permissions: ['debugger', 'tabs', 'storage'], host_permissions: ['http://127.0.0.1/*'],
    background: { service_worker: 'worker.js' }, action: { default_title: 'Nova Chrome 验证助手', default_popup: 'popup.html' },
  }, null, 2)),
  writeFile(join(extension, 'config.json'), JSON.stringify({ origin, fixtureUrl, nonce, token })),
  writeFile(join(extension, 'worker.js'), await readFile(join(root, 'worker.js'))),
  writeFile(join(extension, 'popup.html'), await readFile(join(root, 'popup.html'))),
  writeFile(join(extension, 'popup.js'), await readFile(join(root, 'popup.js'))),
  writeFile(join(extension, 'native_browser_page.js'), await readFile(new URL('../../src-tauri/src/native_browser_page.js', import.meta.url))),
]);
await writeFile(join(output, 'launch.json'), JSON.stringify({ extension, extensionId, fixtureUrl, origin }, null, 2));
console.log(JSON.stringify({ extension, extensionId, fixtureUrl, report: join(output, 'report.json') }, null, 2));
setTimeout(() => { console.log(reported ? 'Probe server finished.' : 'No extension report received within 2 hours.'); server.closeAllConnections(); server.close(); }, 2 * 60 * 60 * 1000);
