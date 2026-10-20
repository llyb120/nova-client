import assert from 'node:assert/strict';
import { test } from 'node:test';
import { readFile, mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createCipheriv, createHash, randomBytes } from 'node:crypto';
import { createServer as httpServer } from 'node:http';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { setTimeout as delay } from 'node:timers/promises';

const source = await readFile(new URL('./workbuddy-acp.mjs', import.meta.url), 'utf8');
const rpc = (id, method, params = {}) => ({ jsonrpc: '2.0', id, method, params });
const event = message => `data: ${JSON.stringify(message)}\r\n\r\n`;
// Same restricted arguments owned by acp.rs; the bridge must preserve even the empty value.
const printArgs = ['--print', '--verbose', '--tools', '', '--strict-mcp-config', '--mcp-config', 'mcp.json',
  '--setting-sources', 'none', '--settings', 'settings.json', '--permission-mode', 'dontAsk',
  '--no-session-persistence', '--max-turns', '1', '--input-format', 'stream-json', '--output-format', 'stream-json',
  '--model', 'model with spaces'];
const printStderr = Buffer.from('worker stderr\r\n分块🐰\0');
const wait = async predicate => {
  const deadline = Date.now() + 5000;
  while (!predicate()) {
    assert.ok(Date.now() < deadline, 'Timed out waiting for fake transport');
    await delay(10);
  }
};

// Load only declarations and replace the stdout sink, not the parser implementation.
const parserSource = source.slice(0, source.indexOf("process.stdout.on('error'"))
  .replace(/async function forward\(message\) \{[\s\S]*?\n\}\n\nasync function relay/,
    'const messages = [];\nasync function forward(message) { messages.push(message); }\n\nasync function relay');
const parser = await import(`data:text/javascript;base64,${Buffer.from(parserSource + '\nexport { relay, jsonBody, messages, lines, LIMIT, INPUT_LIMIT };').toString('base64')}`);
async function* chunks(bytes) { for (const byte of bytes) yield Buffer.from([byte]); }

test('SSE split UTF-8, CRLF, multiline data, multiple events and buffer limits', async () => {
  const message = rpc('server-id', 'session/update', { text: '分块🐰' });
  const wire = `: heartbeat\r\nevent: message\r\ndata: ${JSON.stringify(message).slice(0, -1)},\r\ndata: "extra":true}\r\n\r\n` +
    event({ jsonrpc: '2.0', id: 0, result: null });
  await parser.relay(chunks(Buffer.from(wire)));
  assert.deepEqual(parser.messages, [{ ...message, extra: true }, { jsonrpc: '2.0', id: 0, result: null }]);
  await assert.rejects(parser.relay([Buffer.from('data: ' + 'x'.repeat(parser.LIMIT + 1))]), /limit/);
  await assert.rejects(parser.relay([Buffer.from(('data: ' + 'x'.repeat(1024) + '\n').repeat(parser.LIMIT / 1024))]), /limit/);
  await assert.rejects(parser.relay([Buffer.from('data: {}\n')]), /Truncated/);
  await assert.rejects(parser.jsonBody([Buffer.alloc(parser.LIMIT + 1)]), /limit/);
  const largeLine = Buffer.alloc(parser.LIMIT + 1, 'x');
  for await (const line of parser.lines([largeLine], parser.INPUT_LIMIT)) assert.equal(line.length, largeLine.length);
  await assert.rejects(async () => {
    for await (const line of parser.lines([Buffer.alloc(parser.INPUT_LIMIT + 1)], parser.INPUT_LIMIT)) assert.fail('Oversized input accepted');
  }, /limit/);
  assert.equal(Buffer.byteLength(source) <= 16000, true, 'Rust inline source budget');
});

// Real local pipes/HTTP and a fake worker; never start WorkBuddy or make any upstream/model request.
const fakeWorker = `
import { createServer, createConnection } from 'node:net';
import { readFileSync, writeFileSync } from 'node:fs';
const env = process.env, argv = process.argv.slice(2), print = argv[0] === '--print';
if (env.FAKE_EXIT) process.exit(2);
const keepAlive = setInterval(() => {}, 1000);
const server = createServer(socket => {
  let body = '';
  socket.on('data', chunk => {
    body += chunk;
    if (!body.includes('\\n')) return;
    const message = JSON.parse(body);
    writeFileSync(env.FAKE_REPORT, JSON.stringify({ argv, env, message, pid: process.pid, cwd: process.cwd(),
      ...(print ? { settings: JSON.parse(readFileSync('settings.json', 'utf8')), mcp: JSON.parse(readFileSync('mcp.json', 'utf8')) } : {}) }));
    if (env.FAKE_HANG_CREDENTIALS) return;
    socket.end(JSON.stringify({ ok: true, policy: message.bootstrap.policy }) + '\\n');
    server.close();
    if (print) {
      clearInterval(keepAlive);
      void (async () => {
        const chunks = [];
        for await (const chunk of process.stdin) chunks.push(chunk);
        await new Promise(resolve => setTimeout(resolve, 50)); // Input EOF must not kill the worker.
        process.stdout.write(Buffer.concat(chunks));
        process.stderr.write(Buffer.from(${JSON.stringify(printStderr.toString('base64'))}, 'base64'));
        process.exitCode = Number(env.FAKE_PRINT_EXIT || 0);
      })();
      return;
    }
    const ready = createConnection(env.CODEBUDDY_SIDECAR_READY_SOCKET, () => ready.write(JSON.stringify({ jsonrpc: '2.0', id: 'ready',
      method: 'session.ready', params: { protocolVersion: 1, sessionId: env.CODEBUDDY_SIDECAR_READY_SESSION_ID,
        token: env.CODEBUDDY_SIDECAR_READY_TOKEN, pid: process.pid, port: Number(env.FAKE_PORT) } }) + '\\n'));
    ready.on('data', data => { if (JSON.parse(data).result?.ok !== true) process.exit(3); });
  });
});
server.listen(env.CODEBUDDY_SIDECAR_CREDENTIAL_BOOTSTRAP_SOCKET);
`;
const alive = pid => { try { process.kill(pid, 0); return true; } catch { return false; } };
const sha = data => createHash('sha256').update(data).digest();
const keyId = key => sha(key).toString('hex').slice(0, 16);

// Seal the user key exactly like WorkBuddy's static-v1 keyblob slot (AES-256-GCM, file-framed AAD).
function keyblob(secret, user) {
  const protector = sha(secret), id = keyId(protector), nonce = randomBytes(12);
  const u32 = value => { const buffer = Buffer.alloc(4); buffer.writeUInt32BE(value); return buffer; };
  const text = value => Buffer.concat([u32(Buffer.byteLength(value)), Buffer.from(value)]);
  const cipher = createCipheriv('aes-256-gcm', protector, nonce, { authTagLength: 16 });
  cipher.setAAD(Buffer.concat([Buffer.from('WB-AAD\0'), Buffer.from([1]), text('WBEF1'), text('sym-v1'), u32(1), text(id), Buffer.from([1, 0, 0])]));
  const ciphertext = Buffer.concat([cipher.update(user), cipher.final()]);
  const wrapped = Buffer.from(JSON.stringify({ suite: 1, keyId: id, nonce: nonce.toString('base64'),
    authTag: cipher.getAuthTag().toString('base64'), ciphertext: ciphertext.toString('base64') })).toString('base64');
  return JSON.stringify({ version: 1, keyId: keyId(user), slots: [{ type: 'static-v1', protectorKeyId: id, wrapped }] });
}

async function fixture(t, options = {}) {
  const root = await mkdtemp(join(tmpdir(), 'nova-wb-test-'));
  const print = options.args?.[0] === '--print';
  const f = { root, posts: [], requests: [], messages: [], lifecycle: [], sockets: new Set(), stderr: '', stdout: '', stdoutBytes: [], stderrBytes: [] };
  f.secret = randomBytes(32).toString('base64');
  f.user = randomBytes(32);
  f.developerPublicKey = { alg: 'RSA-OAEP-256', id: 'ab'.repeat(32), key: 'AAAA' };
  const http = httpServer(async (req, res) => {
    f.requests.push({ method: req.method, url: req.url, headers: req.headers });
    res.on('error', () => {});
    if (req.method === 'DELETE') {
      f.lifecycle.push('delete');
      if (!options.hangDelete) res.writeHead(204).end();
    } else if (req.url.endsWith('/connect')) {
      res.setHeader('Content-Type', 'application/json');
      res.end(JSON.stringify({ connectionId: 'connection-test', sessionToken: 'HTTP_SESSION_SECRET' }));
    } else if (req.method === 'GET') {
      f.events = res;
      res.writeHead(200, { 'Content-Type': 'text/event-stream' });
      res.flushHeaders();
      res.on('close', () => { f.eventsClosed = true; });
    } else {
      let body = '';
      for await (const chunk of req) body += chunk;
      const message = JSON.parse(body);
      f.posts.push(message);
      if (options.onPost?.(message, res, f)) return;
      if (message.method && Object.hasOwn(message, 'id')) {
        res.writeHead(200, { 'Content-Type': 'text/event-stream' });
        res.end(event({ jsonrpc: '2.0', id: message.id, result: { ok: true } }));
      } else res.writeHead(204).end();
    }
  });
  http.on('connection', socket => {
    f.sockets.add(socket);
    socket.on('close', () => f.sockets.delete(socket));
  });
  t.after(async () => {
    if (f.child && f.child.exitCode === null) {
      f.child.kill();
      await Promise.race([f.exit, delay(3500)]);
    }
    if (f.report && alive(f.report.pid)) process.kill(f.report.pid);
    for (const socket of f.sockets) socket.destroy();
    await new Promise(resolve => http.close(resolve));
    await rm(root, { recursive: true, force: true });
  });
  const preload = join(root, 'fake-workbuddy.cjs'), cli = join(root, 'fake-cli.mjs'), report = join(root, 'report.json');
  const payload = JSON.stringify({ version: 1, atRestSecretKey: f.secret, atRestDeveloperPublicKey: f.developerPublicKey });
  await writeFile(preload, `process._linkedBinding = name => name === 'electron_browser_workbuddy_storage' ? { loggerGet: () => ${JSON.stringify(payload)} } : undefined;`);
  await writeFile(cli, fakeWorker);
  await writeFile(join(root, 'keyblob'), keyblob(f.secret, f.user));
  if (print) {
    await writeFile(join(root, 'settings.json'), '{"hooks":{},"enabledPlugins":{}}');
    await writeFile(join(root, 'mcp.json'), '{"mcpServers":{}}');
  }
  http.listen(0, '127.0.0.1');
  await once(http, 'listening');
  // Only non-Windows tests lift the platform gate; production intentionally rejects Unix.
  let childSource = process.platform === 'win32' ? source : source.replace("process.platform !== 'win32'", 'false');
  if (options.hangCredentials) childSource = childSource.replace('startupDeadline = Date.now() + 45000;', 'startupDeadline = Date.now() + 1500;');
  f.child = spawn(process.execPath, ['--input-type=module', '-e', childSource, '--', cli, ...(options.args || [])], {
    cwd: root, stdio: ['pipe', 'pipe', 'pipe'],
    env: { ...process.env, WORKBUDDY_CONFIG_DIR: root, NOVA_CONTEXT_TEST: 'forward-context',
      WORKBUDDY_EXE: options.exe ?? process.execPath, NODE_OPTIONS: `--require ${JSON.stringify(preload)}`,
      FAKE_PORT: String(http.address().port), FAKE_REPORT: report, ...(options.workerExit ? { FAKE_EXIT: '1' } : {}),
      FAKE_PRINT_EXIT: String(options.printExit || 0), ...(options.hangCredentials ? { FAKE_HANG_CREDENTIALS: '1' } : {}),
      HTTPS_PROXY: 'http://127.0.0.1:9', CODEBUDDY_CODE_SHELL: 'custom-shell',
      CODEBUDDY_GATEWAY_PASSWORD: 'OLD_PASSWORD', CODEBUDDY_FORCE_LITE_WB_BUNDLE: '0',
      ACC_PRODUCT_CONFIG_PATH: join(root, 'stale-desktop-policy.json'),
      ACC_PRODUCT_CONFIG_V3: '{"agents":[{"name":"cli","models":["stale"]}]}',
      ACC_PRODUCT_CONFIG_V2: '{}', ACC_PRODUCT_CONFIG: '{}' },
  });
  f.exit = once(f.child, 'exit');
  f.close = once(f.child, 'close');
  f.send = message => f.child.stdin.write(JSON.stringify(message) + '\n');
  f.loadReport = async () => (f.report ??= JSON.parse(await readFile(report, 'utf8')));
  if (print) {
    f.child.stdout.on('data', chunk => { f.stdoutBytes.push(chunk); f.stdout += chunk.toString('utf8'); });
    f.child.stderr.on('data', chunk => { f.stderrBytes.push(chunk); f.stderr += chunk.toString('utf8'); });
  } else {
    f.child.stderr.setEncoding('utf8').on('data', text => { f.stderr += text; });
    let tail = '';
    f.child.stdout.setEncoding('utf8').on('data', text => {
      f.stdout += text;
      const split = (tail + text).split('\n');
      tail = split.pop();
      for (const line of split) if (line) f.messages.push(JSON.parse(line));
    });
  }
  f.checkSecrets = () => {
    const output = f.stdout + f.stderr;
    const password = f.report?.env.CODEBUDDY_GATEWAY_PASSWORD;
    for (const secret of [f.secret, sha(f.secret).toString('base64'), f.user.toString('base64'),
      'HTTP_SESSION_SECRET', 'OLD_PASSWORD', password, f.report?.message.token].filter(Boolean)) {
      assert.equal(output.includes(secret), false, 'Secret leaked to stdio');
    }
  };
  return f;
}

test('self-hosted worker: credential bootstrap, env, GET and POST forwarding, concurrent permission reply, cleanup', { timeout: 20000 }, async t => {
  const f = await fixture(t, { onPost(message, res, state) {
    if (message.method === 'session/prompt') {
      state.prompt = res;
      state.promptId = message.id;
      res.writeHead(200, { 'Content-Type': 'text/event-stream' });
      res.write(event(rpc('permission', 'session/request_permission', { options: [] })));
      res.on('close', () => { state.promptClosed = true; });
      return true;
    }
    if (message.id === 'permission') {
      res.writeHead(204).end();
      state.prompt.end(event({ jsonrpc: '2.0', id: state.promptId, result: { stopReason: 'end_turn' } }));
      return true;
    }
  } });
  f.send(rpc(0, 'initialize'));
  f.send({ jsonrpc: '2.0', method: 'client/notification' });
  await wait(() => f.messages.some(m => m.id === 0));
  const { argv, env, message } = await f.loadReport();
  assert.deepEqual(argv, ['--serve', '--port', '0', '--host', '127.0.0.1']);
  assert.match(env.CODEBUDDY_SIDECAR_READY_SESSION_ID, /^nova-[a-f0-9-]{36}$/);
  assert.equal(message.sessionId, env.CODEBUDDY_SIDECAR_READY_SESSION_ID);
  assert.equal(message.token, env.CODEBUDDY_SIDECAR_READY_TOKEN);
  assert.equal(message.pid, f.report.pid);
  const symmetric = sha(f.secret);
  assert.deepEqual(message.bootstrap, {
    version: 1, policy: 'fields',
    symmetricKey: { keyId: keyId(symmetric), keyBase64: symmetric.toString('base64') },
    userKey: { keyId: keyId(f.user), keyBase64: f.user.toString('base64') },
    developerPublicKey: { alg: 'RSA-OAEP-256', id: f.developerPublicKey.id, keyDerBase64: f.developerPublicKey.key },
  });
  assert.equal(env.WORKBUDDY_AT_REST_ENCRYPTION, 'fields');
  assert.equal(env.WORKBUDDY_CONFIG_DIR, env.CODEBUDDY_CONFIG_DIR);
  assert.equal(env.CODEBUDDY_HOST, 'workbuddy-desktop');
  assert.equal(env.NOVA_CONTEXT_TEST, 'forward-context');
  assert.equal(env.HTTPS_PROXY, 'http://127.0.0.1:9');
  assert.equal(env.CODEBUDDY_CODE_SHELL, 'custom-shell');
  assert.equal(env.CODEBUDDY_GATEWAY_AUTH, 'password');
  assert.match(env.CODEBUDDY_GATEWAY_PASSWORD, /^[a-f0-9]{64}$/);
  assert.equal(env.DISABLE_AUTOUPDATER, '1');
  assert.equal(env.CODEBUDDY_FORCE_LITE_WB_BUNDLE, '1');
  for (const key of ['ACC_PRODUCT_CONFIG_PATH', 'ACC_PRODUCT_CONFIG_V3', 'ACC_PRODUCT_CONFIG_V2', 'ACC_PRODUCT_CONFIG']) {
    assert.equal(env[key], undefined, 'Desktop bootstrap model policy must not override the authenticated cloud catalog');
  }
  f.events.write(event({ jsonrpc: '2.0', method: 'session/update', params: { text: 'GET update' } }));
  f.send(rpc('prompt-one', 'session/prompt'));
  await wait(() => f.messages.some(m => m.id === 'permission'));
  assert.equal(f.posts.some(m => m.id === 'permission'), false, 'Never auto-approve');
  const permission = { jsonrpc: '2.0', id: 'permission', result: { outcome: { outcome: 'cancelled' } } };
  f.send(permission);
  await wait(() => f.messages.some(m => m.id === 'prompt-one'));
  assert.deepEqual(f.posts.find(m => m.id === 'permission'), permission);
  assert.ok(f.messages.some(m => m.params?.text === 'GET update'));
  f.send(rpc(2, 'session/prompt'));
  await wait(() => f.promptId === 2);
  const before = Date.now();
  f.send({ jsonrpc: '2.0', method: 'nova/workbuddy/shutdown' });
  assert.equal((await f.exit)[0], 0);
  assert.ok(Date.now() - before < 3500);
  assert.equal(f.posts.some(m => m.method === 'nova/workbuddy/shutdown'), false);
  assert.deepEqual(f.lifecycle, ['delete']);
  await wait(() => !alive(f.report.pid));
  assert.equal(f.eventsClosed, true);
  assert.equal(f.promptClosed, true);
  for (const request of f.requests) {
    assert.equal(request.headers.authorization, `Bearer ${env.CODEBUDDY_GATEWAY_PASSWORD}`);
    assert.equal(request.headers['x-codebuddy-request'], '1');
    if (!request.url.endsWith('/connect')) {
      assert.equal(request.headers['acp-connection-id'], 'connection-test');
      assert.equal(request.headers['acp-session-token'], 'HTTP_SESSION_SECRET');
    }
  }
  assert.equal(f.requests.filter(r => r.method === 'GET').length, 1);
  assert.equal(f.requests.filter(r => r.url.endsWith('/connect')).length, 1);
  f.checkSecrets();
});

test('print reuses credentials and identity, preserves restricted args/cwd and raw stdio, and never opens ACP', { timeout: 10000 }, async t => {
  const f = await fixture(t, { args: printArgs });
  const input = Buffer.concat([Buffer.from('not RPC\r\n分块🐰\0'), Buffer.from([0xff, 0xfe]), Buffer.alloc(128 * 1024, 'x')]);
  f.child.stdin.end(input);
  assert.equal((await f.close)[0], 0);
  assert.deepEqual(Buffer.concat(f.stdoutBytes), input);
  assert.deepEqual(Buffer.concat(f.stderrBytes), printStderr);
  const { argv, env, message, cwd, settings, mcp, pid } = await f.loadReport();
  assert.deepEqual(argv, printArgs);
  assert.equal(cwd, f.root);
  assert.deepEqual(settings, { hooks: {}, enabledPlugins: {} });
  assert.deepEqual(mcp, { mcpServers: {} });
  assert.equal(env.WORKBUDDY_CONFIG_DIR, f.root);
  assert.equal(env.CODEBUDDY_CONFIG_DIR, f.root);
  assert.equal(env.CODEBUDDY_HOST, 'workbuddy-desktop');
  assert.equal(env.DISABLE_AUTOUPDATER, '1');
  assert.equal(env.CODEBUDDY_DEFER_TOOL_LOADING, '0');
  assert.equal(env.WORKBUDDY_AT_REST_ENCRYPTION, 'fields');
  assert.equal(env.HTTPS_PROXY, 'http://127.0.0.1:9');
  assert.equal(env.CODEBUDDY_SIDECAR_READY_SOCKET, undefined);
  assert.equal(env.CODEBUDDY_FORCE_LITE_WB_BUNDLE, '1');
  assert.ok(!Object.keys(env).some(key => /^(NOVA_|ACC_PRODUCT_CONFIG)/i.test(key)));
  assert.equal(message.sessionId, env.CODEBUDDY_SIDECAR_READY_SESSION_ID);
  assert.equal(message.token, env.CODEBUDDY_SIDECAR_READY_TOKEN);
  assert.equal(message.pid, pid);
  assert.equal(message.bootstrap.symmetricKey.keyBase64, sha(f.secret).toString('base64'));
  assert.equal(message.bootstrap.userKey.keyBase64, f.user.toString('base64'));
  for (const value of Object.values(env)) {
    assert.ok(!value.includes(f.secret) && !value.includes(f.user.toString('base64')) && !value.includes(sha(f.secret).toString('base64')));
  }
  assert.equal(f.requests.length, 0);
  assert.deepEqual(f.lifecycle, []);
  assert.equal(alive(pid), false);
  f.checkSecrets();
});

test('print preserves a nonzero worker exit without replacing its output with JSON-RPC', { timeout: 10000 }, async t => {
  const f = await fixture(t, { args: printArgs, printExit: 7 });
  f.child.stdin.end('partial output');
  assert.equal((await f.close)[0], 7);
  assert.equal(f.stdout, 'partial output');
  assert.deepEqual(Buffer.concat(f.stderrBytes), printStderr);
  await f.loadReport();
  assert.equal(f.requests.length, 0);
  f.checkSecrets();
});

for (const failure of ['build key', 'early worker exit', 'credential timeout']) {
  test(`print fails closed on ${failure} and cleans up without leaking credentials`, { timeout: 10000 }, async t => {
    const f = await fixture(t, { args: printArgs,
      ...(failure === 'build key' ? { exe: join(tmpdir(), 'missing-workbuddy.exe') } : {}),
      workerExit: failure === 'early worker exit', hangCredentials: failure === 'credential timeout' });
    f.child.stdin.end();
    assert.notEqual((await f.close)[0], 0);
    assert.equal(f.stdout, '');
    assert.equal(f.requests.length, 0);
    if (failure === 'credential timeout') {
      const { pid } = await f.loadReport();
      assert.equal(alive(pid), false);
    } else await assert.rejects(f.loadReport());
    f.checkSecrets();
  });
}

test('a print token outside the explicit mode position cannot enable stdio forwarding', { timeout: 10000 }, async t => {
  const f = await fixture(t, { args: ['--model', '--print'] });
  f.send(rpc('invalid-args', 'initialize'));
  assert.equal((await f.close)[0], 1);
  assert.equal(f.messages[0].id, 'invalid-args');
  assert.equal(f.messages[0].error.code, -32000);
  await assert.rejects(f.loadReport());
  assert.equal(f.requests.length, 0);
  f.checkSecrets();
});

test('missing WorkBuddy build key never spawns a worker and sanitizes the RPC error', { timeout: 10000 }, async t => {
  const f = await fixture(t, { exe: join(tmpdir(), 'missing-workbuddy.exe') });
  f.send(rpc('original-id', 'initialize'));
  assert.equal((await f.exit)[0], 1);
  assert.equal(f.messages.length, 1);
  assert.equal(f.messages[0].id, 'original-id');
  assert.equal(f.messages[0].error.code, -32000);
  assert.match(f.messages[0].error.message, /请确认已安装 WorkBuddy 并至少登录过一次/);
  await assert.rejects(f.loadReport());
  assert.equal(f.requests.length, 0);
  f.checkSecrets();
});

test('worker exiting during startup fails fast; no RPC error for client responses', { timeout: 10000 }, async t => {
  const f = await fixture(t, { workerExit: true });
  f.send({ jsonrpc: '2.0', id: 'server-request', result: {} });
  const before = Date.now();
  assert.equal((await f.exit)[0], 1);
  assert.ok(Date.now() - before < 5000);
  assert.deepEqual(f.messages, []);
  assert.equal(f.requests.length, 0);
});

test('SSE EOF without an RPC reply reports the original request instead of hanging', { timeout: 10000 }, async t => {
  const f = await fixture(t, { onPost(message, res) {
    res.writeHead(200, { 'Content-Type': 'text/event-stream' });
    res.end(event({ jsonrpc: '2.0', method: 'session/update', params: {} }));
    return true;
  } });
  f.send(rpc('missing-reply', 'initialize'));
  assert.equal((await f.exit)[0], 1);
  assert.equal(f.messages.find(m => m.id === 'missing-reply')?.error.code, -32000);
  assert.deepEqual(f.lifecycle, ['delete']);
  const { pid } = await f.loadReport();
  await wait(() => !alive(pid));
});

test('EOF aborts live streams; stalled DELETE cannot prevent worker kill', { timeout: 10000 }, async t => {
  const f = await fixture(t, { hangDelete: true });
  f.send(rpc(1, 'initialize'));
  await wait(() => f.messages.length);
  const { pid } = await f.loadReport();
  const before = Date.now();
  f.child.stdin.end();
  assert.equal((await f.exit)[0], 1);
  assert.ok(Date.now() - before < 3500);
  assert.deepEqual(f.lifecycle, ['delete']);
  assert.equal(f.eventsClosed, true);
  await wait(() => !alive(pid));
  f.checkSecrets();
});
